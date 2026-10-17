// The Feed tab: For you, the posts the user's agents find for the followed topics.
//
// It reads like a social feed: each topic is watched by an agent on a routine of its own (local_backend/feed.rs),
// each post says which agent shared it, a quiet line says when they last looked, and posts that arrive while the list
// is open wait behind an "N new posts" pill instead of pushing what's being read down. The refresh button runs the
// topics' routines, only those that didn't just run (`ifStale`).
//
// One measured list (GPUI's `list`), like the chat: only what's on screen is laid out and drawn, so the covers of
// posts scrolled away can leave the picture cache (pictures.rs) instead of all forty staying decoded. "Ask <agent>"
// opens the agent's conversation beside the posts (feed_chat.rs); a selected topic has its own Refresh, which runs its
// routine now.
//
// A post can be a glance an agent posted, drawn live (feed_glance.rs); the Glances chip beside For you lists only those.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Corner, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, list, prelude::FluentBuilder as _,
    px,
};
use gpui_component::button::{Button as MenuButton, ButtonRounded, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, Sizable as _};
use serde_json::json;

use super::feed_chat::{self, FeedChat, FeedChatEvent};
#[path = "feed_glance.rs"]
mod feed_glance;
use super::feed_common::{self as common, FeedMine, FeedPost, FeedTopic, Notice, PostHost};
use super::feed_rows::{self, RowSig};
use crate::backend;
use crate::format;
use crate::nav::{self, Route};
use crate::theme::{ActiveTheme as _, Typography as _, radius, space};
use crate::ui::{self, Button, IconButton};

/// One entry of the list.
#[derive(Clone, PartialEq)]
enum Row {
    Header,
    /// The selected topic: its Refresh and what the last one did.
    TopicBar(String),
    Post(String),
    End,
    /// Loading, an error, or nothing to show.
    Placeholder,
}

impl Row {
    /// Stays the same while the row shows the same thing, so the list keeps its measured heights and place.
    fn key(&self) -> String {
        match self {
            Row::Header => "header".into(),
            Row::TopicBar(key) => format!("topic:{key}"),
            Row::Post(id) => format!("post:{id}"),
            Row::End => "end".into(),
            Row::Placeholder => "placeholder".into(),
        }
    }
}

/// A topic's Refresh asked for, until the topic says it's running (or this long passes).
const REFRESH_PENDING: Duration = Duration::from_secs(15);
/// How often "Updated 5 min ago" is drawn again while nothing else changes.
const FRESHNESS_TICK: Duration = Duration::from_secs(30);
/// Refresh turns at least once (one turn of the icon) so a quick answer still shows it was pressed.
const MIN_SPIN: Duration = Duration::from_millis(900);
/// A check Refresh started that `feed:mine` never reports ending stops being waited for after this.
const WATCH_LIMIT: Duration = Duration::from_secs(5 * 60);

/// A check Refresh started, until `feed:mine` reports it over: then it says what the check found.
struct CheckWatch {
    /// The posts the Feed had when it started.
    before: HashSet<String>,
    started_ms: f64,
    started: Instant,
    seen_running: bool,
}

pub struct FeedScreen {
    feed: Option<Result<FeedMine, String>>,
    topics: Option<Result<Vec<FeedTopic>, String>>,
    topic: Option<String>,
    /// The Glances chip: only glance posts are listed.
    glances_only: bool,
    /// The glance posts' pages and picks (feed_glance.rs).
    glances: feed_glance::FeedGlances,
    like_overrides: HashMap<String, bool>,
    save_overrides: HashMap<String, bool>,
    /// Posts hidden this session, with the confirmation their undo row shows.
    hidden_now: HashMap<String, String>,
    expanded: HashSet<String>,
    /// The posts the list shows (None until the first answer), and those that came since, waiting behind the pill.
    shown: Option<HashSet<String>>,
    arrived: HashSet<String>,
    /// Refresh was pressed and hasn't answered (or turned once) yet; then a check it started, until it ends.
    refreshing: bool,
    refresh_since: Option<Instant>,
    check_watch: Option<CheckWatch>,
    /// Topics whose Refresh was just asked for, and why the last ask didn't start one.
    refresh_pending: HashMap<String, Instant>,
    refresh_errors: HashMap<String, String>,
    notice: Notice,
    list: ListState,
    rows: Vec<Row>,
    /// The chat beside the posts, and whether it's open (it stays when closed, keeping each post's chat).
    chat: Option<Entity<FeedChat>>,
    chat_open: bool,
    _chat_sub: Option<Subscription>,
    /// Scripted checks: what the backend answers a Refresh with instead of asking it, and Refresh asks sent.
    #[cfg(feature = "dev")]
    mock_refresh: Option<Result<String, String>>,
    /// The feed fixture: what the page's Refresh answers (None: the Feed is up to date).
    #[cfg(feature = "dev")]
    mock_check: Option<Result<String, String>>,
    #[cfg(feature = "dev")]
    refresh_asks: usize,
    /// Scripted checks: the topics stand in for `feed:topicsMine` until the check ends.
    #[cfg(feature = "dev")]
    topics_pinned: bool,
    /// The feed fixture (feed_fixture.rs): made-up posts stand in for `feed:mine`.
    #[cfg(feature = "dev")]
    feed_pinned: bool,
    /// The feed fixture's agents, for its Ask panel.
    #[cfg(feature = "dev")]
    fixture_agents: Vec<super::agents::AgentRow>,
    #[cfg(feature = "dev")]
    _mock_task: Option<Task<()>>,
    _spin_task: Option<Task<()>>,
    /// What each row showed when the list last measured it (feed_rows::sync_list).
    row_sigs: Vec<RowSig>,
    /// Keyboard focus for the topic chips, More and Show more (Tab reaches them; Return or Space acts).
    focus: RefCell<HashMap<String, FocusHandle>>,
    _tick: Task<()>,
    _subs: Vec<Task<()>>,
}

impl PostHost for FeedScreen {
    fn notice(&mut self) -> &mut Notice {
        &mut self.notice
    }
}

impl FeedScreen {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let feed =
            backend::subscribe(cx, "feed:mine", json!({}), |this: &mut Self, result: Result<FeedMine, String>, cx| {
                #[cfg(feature = "dev")]
                if this.feed_pinned {
                    return;
                }
                if let Ok(feed) = &result {
                    let liked: HashMap<&str, bool> =
                        feed.posts.iter().map(|post| (post.id.as_str(), post.liked)).collect();
                    let saved: HashMap<&str, bool> =
                        feed.posts.iter().map(|post| (post.id.as_str(), post.saved)).collect();
                    common::settle_overrides(&mut this.like_overrides, |id| liked.get(id).copied());
                    common::settle_overrides(&mut this.save_overrides, |id| saved.get(id).copied());
                }
                this.feed = Some(result);
                this.forget_old_glances();
                this.note_arrivals();
                this.watch_check(cx);
                cx.notify();
            });
        let topics = backend::subscribe(
            cx,
            "feed:topicsMine",
            json!({}),
            |this: &mut Self, result: Result<Vec<FeedTopic>, String>, cx| {
                #[cfg(feature = "dev")]
                if this.topics_pinned {
                    return;
                }
                this.set_topics(result, cx);
            },
        );
        FeedScreen {
            feed: None,
            topics: None,
            topic: None,
            glances_only: false,
            glances: feed_glance::FeedGlances::default(),
            like_overrides: HashMap::new(),
            save_overrides: HashMap::new(),
            hidden_now: HashMap::new(),
            expanded: HashSet::new(),
            shown: None,
            arrived: HashSet::new(),
            refreshing: false,
            refresh_since: None,
            check_watch: None,
            refresh_pending: HashMap::new(),
            refresh_errors: HashMap::new(),
            notice: Notice::default(),
            list: ListState::new(0, ListAlignment::Top, px(1200.)),
            rows: Vec::new(),
            chat: None,
            chat_open: false,
            _chat_sub: None,
            #[cfg(feature = "dev")]
            mock_refresh: None,
            #[cfg(feature = "dev")]
            mock_check: None,
            #[cfg(feature = "dev")]
            refresh_asks: 0,
            #[cfg(feature = "dev")]
            topics_pinned: false,
            #[cfg(feature = "dev")]
            feed_pinned: false,
            #[cfg(feature = "dev")]
            fixture_agents: Vec::new(),
            #[cfg(feature = "dev")]
            _mock_task: None,
            _spin_task: None,
            row_sigs: Vec::new(),
            focus: RefCell::new(HashMap::new()),
            _tick: cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(FRESHNESS_TICK).await;
                    let tick = this.update(cx, |this, cx| {
                        if this.check_watch.as_ref().is_some_and(|watch| watch.started.elapsed() > WATCH_LIMIT) {
                            this.check_watch = None;
                        }
                        cx.notify()
                    });
                    if tick.is_err() {
                        break;
                    }
                }
            }),
            _subs: vec![feed, topics],
        }
    }

    /// After `feed:mine` answers: the first answer is what the list shows; posts that come later wait behind the pill
    /// (a list that showed nothing takes them straight away).
    fn note_arrivals(&mut self) {
        let Some(Ok(feed)) = &self.feed else { return };
        let all = feed.posts.iter().map(|post| post.id.clone());
        match &mut self.shown {
            None => self.shown = Some(all.collect()),
            Some(shown) => {
                self.arrived = common::arrived_posts(&feed.posts, shown);
                if self.arrived.is_empty() {
                    shown.extend(all);
                }
            }
        }
    }

    /// The pill: the waiting posts join the list and the list goes to the top, where they are.
    fn show_arrived(&mut self, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown {
            shown.extend(self.arrived.drain());
        }
        self.list.scroll_to(ListOffset::default());
        cx.notify();
    }

    /// The posts listed now: the topic's (only glances, with the Glances chip), less hidden ones and those waiting
    /// behind the pill.
    fn listed_posts<'a>(&self, posts: &'a [FeedPost]) -> Vec<&'a FeedPost> {
        let keep: HashSet<String> = self.hidden_now.keys().cloned().collect();
        common::visible_posts(posts, self.topic.as_deref(), &keep)
            .into_iter()
            .filter(|post| !self.arrived.contains(&post.id) && (!self.glances_only || post.glance.is_some()))
            .collect()
    }

    /// Whether the Feed has a glance to list (the Glances chip shows then, even with no topics followed).
    fn has_glances(&self) -> bool {
        matches!(&self.feed, Some(Ok(feed)) if feed.posts.iter().any(|post| post.glance.is_some() && post.hidden.is_none()))
    }

    /// The Glances chip: only glance posts, of every topic.
    fn select_glances(&mut self, cx: &mut Context<Self>) {
        self.topic = None;
        self.glances_only = true;
        self.list.scroll_to(ListOffset::default());
        cx.notify();
    }

    /// Waiting posts the pill counts: those the topic filter would show.
    fn arrived_count(&self) -> usize {
        let Some(Ok(feed)) = &self.feed else { return 0 };
        feed.posts
            .iter()
            .filter(|post| {
                self.arrived.contains(&post.id) && self.topic.as_ref().is_none_or(|key| &post.topic_key == key)
            })
            .filter(|post| !self.glances_only || post.glance.is_some())
            .count()
    }

    fn set_topics(&mut self, result: Result<Vec<FeedTopic>, String>, cx: &mut Context<Self>) {
        if let Ok(topics) = &result {
            // A topic that was unfollowed stops filtering.
            if let Some(topic) = &self.topic
                && !topics.iter().any(|row| &row.topic_key == topic)
            {
                self.topic = None;
            }
            // Its run has started (or the topic is gone): the ask is answered.
            self.refresh_pending.retain(|key, _| topics.iter().any(|row| &row.topic_key == key && !row.refreshing));
        }
        self.topics = Some(result);
        cx.notify();
    }

    fn like(&mut self, post_id: String, liked: bool, cx: &mut Context<Self>) {
        self.like_overrides.insert(post_id.clone(), liked);
        cx.notify();
        let id = post_id.clone();
        backend::call(
            cx,
            "feed:setLikedMine",
            json!({ "postId": post_id, "liked": liked }),
            move |this: &mut Self, result: Result<serde_json::Value, String>, cx| {
                if result.is_err() {
                    this.like_overrides.remove(&id);
                    cx.notify();
                }
            },
        );
    }

    fn save(&mut self, post_id: String, saved: bool, cx: &mut Context<Self>) {
        self.save_overrides.insert(post_id.clone(), saved);
        cx.notify();
        let id = post_id.clone();
        backend::call(
            cx,
            "feed:setSavedMine",
            json!({ "postId": post_id, "saved": saved }),
            move |this: &mut Self, result: Result<serde_json::Value, String>, cx| match result {
                Ok(_) => common::notify(this, if saved { "Saved to Notes." } else { "Removed from Notes." }, cx),
                Err(_) => {
                    this.save_overrides.remove(&id);
                    common::notify(this, "Couldn't save that. Try again.", cx);
                }
            },
        );
    }

    fn hide(&mut self, post_id: String, topic_label: &str, reason: &'static str, cx: &mut Context<Self>) {
        self.hidden_now.insert(post_id.clone(), common::hide_confirmation(reason, topic_label));
        cx.notify();
        let id = post_id.clone();
        backend::call(
            cx,
            "feed:hideMine",
            json!({ "postId": post_id, "reason": reason }),
            move |this: &mut Self, result: Result<serde_json::Value, String>, cx| {
                if result.is_err() {
                    this.hidden_now.remove(&id);
                    common::notify(this, "Couldn't hide that. Try again.", cx);
                }
            },
        );
    }

    fn undo_hide(&mut self, post_id: String, cx: &mut Context<Self>) {
        self.hidden_now.remove(&post_id);
        cx.notify();
        backend::fire(cx, "feed:unhideMine", json!({ "postId": post_id }));
    }

    /// The refresh button: posts waiting behind the pill come in, and the agents check for more if the last check
    /// isn't a moment old (`ifStale`; a fresh Feed runs nothing and costs no check). It turns until the answer (and at
    /// least once), and on through a check it started, which then says what it found.
    fn check_now(&mut self, cx: &mut Context<Self>) {
        if !self.arrived.is_empty() {
            self.show_arrived(cx);
        }
        if self.refreshing || self.check_watch.is_some() {
            return;
        }
        self.refreshing = true;
        self.refresh_since = Some(Instant::now());
        cx.notify();
        #[cfg(feature = "dev")]
        if self.feed_pinned {
            // The fixture: no backend; it answers what the check set (by default, that the Feed is up to date).
            let answer = self.mock_check.clone().unwrap_or_else(|| Ok("fresh".into()));
            self._mock_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                this.update(cx, |this, cx| this.checked(answer, cx)).ok();
            }));
            return;
        }
        // A Feed with regular checks looks again only when it's stale; a once-a-day Feed has no recent check to be fresh
        // from.
        let mut args = json!({});
        if self.settings().is_some_and(|settings| settings.interval_minutes.is_some()) {
            args["ifStale"] = json!(true);
        }
        backend::call(cx, "feed:refreshMine", args, |this: &mut Self, result: Result<String, String>, cx| {
            this.checked(result, cx)
        });
    }

    /// Refresh's answer. "fresh" ran nothing and says so; "started" (or one already running) is watched to its end.
    fn checked(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        match &result {
            Ok(status) if status == "started" || status == "running" => {
                let before = match &self.feed {
                    Some(Ok(feed)) => feed.posts.iter().map(|post| post.id.clone()).collect(),
                    _ => HashSet::new(),
                };
                let seen_running = self.settings().is_some_and(|settings| settings.running);
                self.check_watch =
                    Some(CheckWatch { before, started_ms: format::now_ms(), started: Instant::now(), seen_running });
                common::notify(self, common::refresh_message(status), cx);
            }
            Ok(status) => common::notify(self, common::refresh_message(status), cx),
            Err(_) => common::notify(self, "Couldn't check for new posts. Try again.", cx),
        }
        let left = self.refresh_since.map(|since| MIN_SPIN.saturating_sub(since.elapsed())).unwrap_or_default();
        if left.is_zero() {
            self.refreshing = false;
            cx.notify();
            return;
        }
        self._spin_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(left).await;
            this.update(cx, |this, cx| {
                this.refreshing = false;
                cx.notify();
            })
            .ok();
        }));
    }

    /// After `feed:mine` answers: a check Refresh started that is over now says what it found.
    fn watch_check(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.settings().cloned() else { return };
        let Some(watch) = &mut self.check_watch else { return };
        if settings.running {
            watch.seen_running = true;
            return;
        }
        // Over: seen running and not now, or finished since it started (the running state can come and go between answers).
        if !watch.seen_running && settings.last_run_at.is_none_or(|at| at < watch.started_ms) {
            return;
        }
        let found = match &self.feed {
            Some(Ok(feed)) => {
                feed.posts.iter().filter(|post| !watch.before.contains(&post.id) && post.hidden.is_none()).count()
            }
            _ => 0,
        };
        self.check_watch = None;
        common::notify(self, common::check_finished_message(&settings, found), cx);
    }

    fn settings(&self) -> Option<&common::FeedSettings> {
        self.feed.as_ref().and_then(|feed| feed.as_ref().ok()).and_then(|feed| feed.settings.as_ref())
    }

    fn topic_row(&self, key: &str) -> Option<&FeedTopic> {
        match &self.topics {
            Some(Ok(topics)) => topics.iter().find(|topic| topic.topic_key == key),
            _ => None,
        }
    }

    fn refresh_busy(&self, key: &str) -> bool {
        self.refresh_pending.get(key).is_some_and(|at| at.elapsed() < REFRESH_PENDING)
            || self.topic_row(key).is_some_and(|topic| topic.refreshing)
    }

    /// New research on one topic (`feed:refreshTopicMine`). Clicks while it's asked for or running do nothing; the
    /// posts already there stay.
    fn refresh_topic(&mut self, key: String, cx: &mut Context<Self>) {
        if self.refresh_busy(&key) {
            return;
        }
        self.refresh_pending.insert(key.clone(), Instant::now());
        self.refresh_errors.remove(&key);
        #[cfg(feature = "dev")]
        {
            self.refresh_asks += 1;
        }
        cx.notify();
        // Scripted checks answer for the backend (smoke.rs `feed-refresh`): no research runs.
        #[cfg(feature = "dev")]
        if let Some(answer) = self.mock_refresh.clone() {
            self._mock_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(600)).await;
                this.update(cx, |this, cx| this.refresh_answered(key, answer, cx)).ok();
            }));
            return;
        }
        backend::call(
            cx,
            "feed:refreshTopicMine",
            json!({ "topicKey": key }),
            move |this: &mut Self, result: Result<String, String>, cx| this.refresh_answered(key, result, cx),
        );
    }

    fn refresh_answered(&mut self, key: String, result: Result<String, String>, cx: &mut Context<Self>) {
        let label = self.topic_row(&key).map(|topic| topic.label.clone()).unwrap_or_default();
        match result {
            Ok(status) => {
                if !matches!(status.as_str(), "started" | "running") {
                    self.refresh_pending.remove(&key);
                    self.refresh_errors.insert(key.clone(), common::topic_refresh_message(&status, &label));
                }
                common::notify(self, common::topic_refresh_message(&status, &label), cx);
            }
            Err(error) => {
                self.refresh_pending.remove(&key);
                self.refresh_errors.insert(key, common::error_text(&error, "Couldn't refresh that topic. Try again."));
            }
        }
        cx.notify();
    }

    fn select_topic(&mut self, topic: Option<String>, cx: &mut Context<Self>) {
        self.topic = topic;
        self.glances_only = false;
        self.list.scroll_to(ListOffset::default());
        cx.notify();
    }

    fn toggle_more(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    fn open_settings(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        nav::push(Route::FeedSettings, window, cx);
    }

    /// "Ask <agent>": the agent's conversation beside the posts, on this post; while it's open, "Add to chat" puts another
    /// post in it, and Ask on a post already in it closes it (its draft kept). Sends nothing.
    fn ask(&mut self, post: FeedPost, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat.clone().filter(|chat| self.chat_open && chat.read(cx).shows(&post.id)) {
            chat.update(cx, |chat, cx| chat.hidden(cx));
            self.chat_open = false;
            cx.notify();
            return;
        }
        let chat = match &self.chat {
            Some(chat) => chat.clone(),
            None => {
                let chat = cx.new(|cx| FeedChat::new(window, cx));
                // The feed fixture has no backend to open an agent's conversation: a chat of no agent's stands in.
                #[cfg(feature = "dev")]
                if self.feed_pinned {
                    let agents = self.fixture_agents.clone();
                    let ask = agents.iter().find(|agent| agent.main).map(|agent| agent.id.clone());
                    chat.update(cx, |chat, _| chat.drive_pinned(agents, ask));
                }
                self._chat_sub = Some(cx.subscribe(&chat, |this, _, event: &FeedChatEvent, cx| match event {
                    FeedChatEvent::Close => {
                        this.chat_open = false;
                        cx.notify();
                    }
                }));
                self.chat = Some(chat.clone());
                chat
            }
        };
        self.chat_open = true;
        chat.update(cx, |chat, cx| chat.show(post, window, cx));
        cx.notify();
    }

    /// What a post's Ask Filo does now: (in the chat on screen, adds to it).
    fn ask_state(&self, post_id: &str, cx: &Context<Self>) -> (bool, bool) {
        match self.chat.as_ref().filter(|_| self.chat_open) {
            Some(chat) => {
                let chat = chat.read(cx);
                let shown = chat.shows(post_id);
                (shown, !shown && chat.adding())
            }
            None => (false, false),
        }
    }

    fn post(&self, id: &str) -> Option<&FeedPost> {
        match &self.feed {
            Some(Ok(feed)) => feed.posts.iter().find(|post| post.id == id),
            _ => None,
        }
    }

    /// The list's width: the page right of the sidebar, less the chat when it's open.
    fn pane_width(&self) -> f32 {
        let chat = if self.chat_open { feed_chat::width(ui::content_width()) + 1. } else { 0. };
        ui::content_width() - chat
    }

    /// The posts' column width.
    fn column_width(&self) -> f32 {
        ui::column_width_in(self.pane_width())
    }

    // --- Rows ---

    /// The rows, each with a hash of what sets its height (feed_rows::RowSig).
    fn build_rows(&self, cx: &App) -> Vec<(Row, u64)> {
        let header = feed_rows::sig((
            self.topic.is_some(),
            self.settings().and_then(|settings| common::freshness_line(settings, format::now_ms())).is_some(),
            matches!(&self.topics, Some(Ok(topics)) if !topics.is_empty()),
            (self.has_glances(), self.glances_only),
        ));
        let mut rows = vec![(Row::Header, header)];
        let (Some(Ok(feed)), Some(Ok(topics))) = (&self.feed, &self.topics) else {
            let state = (self.feed.as_ref().map(|f| f.is_ok()), self.topics.as_ref().map(|t| t.is_ok()));
            rows.push((Row::Placeholder, feed_rows::sig(format!("{state:?}"))));
            return rows;
        };
        if let Some(key) = &self.topic {
            let topic = self.topic_row(key);
            let state = topic.map(|topic| {
                (
                    topic.label.clone(),
                    topic.refreshing,
                    topic.last_refresh_error.clone(),
                    topic.last_refresh_post_count.map(|count| count as i64),
                )
            });
            rows.push((
                Row::TopicBar(key.clone()),
                feed_rows::sig((state, self.refresh_busy(key), self.refresh_errors.get(key))),
            ));
        }
        let posts = self.listed_posts(&feed.posts);
        let settings = feed.settings.as_ref();
        if posts.is_empty() {
            let state = (topics.len(), settings.map(|s| (s.running, s.last_run_error.clone())));
            rows.push((Row::Placeholder, feed_rows::sig(format!("{state:?}"))));
        } else {
            let count = posts.len();
            for post in posts {
                let cover_failed =
                    post.image_url.as_deref().is_some_and(|url| crate::pictures::state(url, cx) == "failed");
                let sig = feed_rows::sig((
                    (&post.title, &post.note, &post.summary, &post.points, &post.image_url, cover_failed),
                    (
                        &post.kind,
                        post.event_starts_at.map(f64::to_bits),
                        post.reminded,
                        &post.source_name,
                        &post.author,
                    ),
                    (self.expanded.contains(&post.id), self.hidden_now.get(&post.id)),
                    self.glance_sig(post),
                ));
                rows.push((Row::Post(post.id.clone()), sig));
            }
            let end = (
                count,
                settings.map(|s| s.interval_minutes.map(f64::to_bits)),
                self.topic.is_some(),
                self.glances_only,
            );
            rows.push((Row::End, feed_rows::sig(format!("{end:?}"))));
        }
        rows
    }

    /// Updates the list for the new rows: only the ones that came, went or changed are measured again, and the row
    /// being read stays where it is on screen (feed_rows::sync_list).
    fn sync_rows(&mut self, cx: &App) {
        let built = self.build_rows(cx);
        let sigs: Vec<RowSig> = built.iter().map(|(row, sig)| RowSig { key: row.key(), sig: *sig }).collect();
        if sigs == self.row_sigs {
            return;
        }
        feed_rows::sync_list(&self.list, &self.row_sigs, &sigs);
        self.rows = built.into_iter().map(|(row, _)| row).collect();
        self.row_sigs = sigs;
    }

    fn focus_handle(&self, key: &str, cx: &App) -> FocusHandle {
        self.focus.borrow_mut().entry(key.to_string()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone()
    }

    /// `element` the keyboard reaches too: Tab focuses it (a ring shows), and Return or Space click it (GPUI turns
    /// them into a click on the focused element, so its `on_click` serves both).
    fn keyboard(
        &self,
        key: &str,
        element: gpui::Stateful<gpui::Div>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focus = self.focus_handle(key, cx);
        let focused = focus.is_focused(window);
        let colors = *cx.colors();
        element
            .track_focus(&focus)
            .px(px(space::XXS))
            .mx(px(-space::XXS))
            .rounded(px(radius::MD))
            .border_1()
            .border_color(if focused { colors.accent } else { gpui::transparent_black() })
    }

    fn render_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index).cloned() else { return div().into_any_element() };
        let now = format::now_ms();
        let content = match &row {
            Row::Header => self.render_header(window, cx),
            Row::TopicBar(key) => self.render_topic_bar(key, now, cx),
            Row::Post(id) => match self.post(id).cloned() {
                Some(post) if post.glance.is_some() => {
                    div().pb(px(space::XXXL)).child(self.render_glance_post(&post, now, window, cx)).into_any_element()
                }
                Some(post) => {
                    div().pb(px(space::XXXL)).child(self.render_post(&post, now, window, cx)).into_any_element()
                }
                None => div().into_any_element(),
            },
            Row::End => self.render_end(cx),
            Row::Placeholder => self.render_placeholder(cx),
        };
        let last = index + 1 == self.rows.len();
        let column = ui::page_column().when(last, |this| this.pb(px(48.))).child(content);
        // A list row is laid out on its own, so the column is centred in a row as wide as the list.
        div()
            .w(px(self.pane_width()))
            .flex()
            .justify_center()
            .child(if last {
                crate::smoke::tracked("feed-end", column.children(crate::smoke::page_end("feed")))
            } else {
                column.into_any_element()
            })
            .into_any_element()
    }

    // --- Header ---

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let topics = match &self.topics {
            Some(Ok(topics)) => Some(topics.clone()),
            _ => None,
        };
        let settings = self.settings();
        let checking =
            self.refreshing || self.check_watch.is_some() || settings.is_some_and(|settings| settings.running);
        let freshness = settings.and_then(|settings| common::freshness_line(settings, format::now_ms()));
        div()
            .flex()
            .flex_col()
            .gap(px(space::LG))
            .mb(px(space::XL))
            .child(ui::page_header(
                "Feed",
                vec![
                    crate::smoke::tracked(
                        "feed-refresh",
                        IconButton::new("feed-refresh", "rotate-cw")
                            .tooltip(if checking { "Checking for new posts…" } else { "Check for new posts" })
                            .spinning(checking)
                            .on_click(cx.listener(|this, _, _, cx| this.check_now(cx))),
                    )
                    .into_any_element(),
                    IconButton::new("feed-settings", "sliders-horizontal")
                        .tooltip("Feed settings")
                        .on_click(cx.listener(Self::open_settings))
                        .into_any_element(),
                ],
            ))
            .when_some(topics.filter(|topics| !topics.is_empty() || self.has_glances()), |this, topics| {
                this.child(self.render_topic_row(&topics, window, cx))
            })
            .when_some(freshness, |this, line| {
                this.child(
                    div()
                        .id("feed-freshness")
                        .mt(px(-space::SM))
                        .footnote()
                        .text_color(colors.text_tertiary)
                        .truncate()
                        .child(line),
                )
            })
            .into_any_element()
    }

    fn render_topic_row(&self, topics: &[FeedTopic], window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let covers = self
            .feed
            .as_ref()
            .and_then(|feed| feed.as_ref().ok())
            .map(|feed| common::topic_covers(&feed.posts))
            .unwrap_or_default();

        let bubble = |id: SharedString, label: String, selected: bool, inner: AnyElement| {
            div()
                .id(id)
                .flex_none()
                .w(px(68.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(space::SM + space::XXS))
                .cursor_pointer()
                // The image (56 px) sits 4 px inside the ring; the ring is drawn last so the
                // image's corner frame can't cover it.
                .child(
                    div()
                        .relative()
                        .size(px(64.))
                        .flex_none()
                        .child(
                            div()
                                .absolute()
                                .top(px(4.))
                                .left(px(4.))
                                .size(px(56.))
                                .rounded(px(28.))
                                .bg(colors.surface)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(inner),
                        )
                        .child(div().absolute().inset_0().rounded(px(32.)).map(|this| {
                            if selected {
                                this.border_2().border_color(colors.accent)
                            } else {
                                this.border_1().border_color(colors.border)
                            }
                        })),
                )
                .child(
                    div()
                        .w(px(68.))
                        .text_center()
                        .micro()
                        .when(selected, |this| this.semibold().text_color(colors.text))
                        .when(!selected, |this| this.text_color(colors.text_secondary))
                        .truncate()
                        .child(label),
                )
        };

        let mut row = div().id("feed-topics").w_full().flex().gap(px(space::MD)).overflow_x_scroll().child(
            crate::smoke::tracked(
                "topic-all",
                self.keyboard(
                    "topic-all",
                    bubble(
                        "feed-topic-all".into(),
                        "For you".into(),
                        self.topic.is_none() && !self.glances_only,
                        ui::filo_mark(24., cx).into_any_element(),
                    ),
                    window,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.select_topic(None, cx))),
            ),
        );
        // Glances: only the live visuals agents posted, from every topic.
        if self.has_glances() || self.glances_only {
            row = row.child(crate::smoke::tracked(
                "topic-glances",
                self.keyboard(
                    "topic-glances",
                    bubble(
                        "feed-topic-glances".into(),
                        "Glances".into(),
                        self.glances_only,
                        ui::icon_sized("chart-column-stacked", 24., colors.text_secondary).into_any_element(),
                    ),
                    window,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.select_glances(cx))),
            ));
        }
        for (index, topic) in topics.iter().enumerate() {
            // A picture that couldn't load shows the letter, like a topic without one.
            let cover = covers.get(&topic.topic_key).filter(|url| crate::pictures::state(url, cx) != "failed");
            let inner = match cover {
                // Exact half of the 56 px inner circle: images ignore oversized radii.
                Some(cover) => common::cover_image(cover, 28., colors.background).into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(28.))
                    .bg(colors.accent_soft)
                    .title3()
                    .text_color(colors.on_accent_soft)
                    .child(topic.label.trim().chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default())
                    .into_any_element(),
            };
            let key = topic.topic_key.clone();
            // A refresh that failed shows on its chip too, not only when the topic is open.
            let failed =
                !topic.refreshing && topic.last_refresh_error.as_deref().is_some_and(|error| !error.is_empty());
            let chip = bubble(
                SharedString::from(format!("feed-topic-{index}")),
                topic.label.clone(),
                self.topic.as_deref() == Some(topic.topic_key.as_str()),
                inner,
            )
            .relative()
            .when(failed, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(4.))
                        .size(px(12.))
                        .rounded(px(radius::FULL))
                        .bg(colors.danger)
                        .border_2()
                        .border_color(colors.background),
                )
            });
            row = row.child(crate::smoke::tracked(
                format!("topic-{index}"),
                self.keyboard(&format!("topic-{}", topic.topic_key), chip, window, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_topic(Some(key.clone()), cx))),
            ));
        }
        row.child(
            self.keyboard(
                "topic-add",
                bubble(
                    "feed-topic-add".into(),
                    "Add".into(),
                    false,
                    ui::icon_sized("plus", 24., colors.text_secondary).into_any_element(),
                ),
                window,
                cx,
            )
            .on_click(cx.listener(Self::open_settings)),
        )
        .into_any_element()
    }

    /// The selected topic: its name, what its Refresh is doing or last did, and the Refresh button.
    fn render_topic_bar(&self, key: &str, now: f64, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(topic) = self.topic_row(key).cloned() else { return div().into_any_element() };
        let busy = self.refresh_busy(key);
        let (line, failed) = match self.refresh_errors.get(key).filter(|_| !busy) {
            Some(error) => (error.clone(), true),
            None => common::topic_refresh_status(&topic, busy, now),
        };
        let refresh_key = key.to_string();
        div()
            .flex()
            .items_center()
            .gap(px(space::MD))
            .mb(px(space::XL))
            .pl(px(space::LG))
            .pr(px(space::MD))
            .py(px(space::SM + space::XXS))
            .rounded(px(radius::LG))
            .bg(colors.surface)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(space::XXS))
                    .child(div().w_full().subhead().semibold().truncate().child(topic.label.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XS))
                            .when(busy, |this| this.child(ui::spinner(12., cx)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .footnote()
                                    .text_color(if failed { colors.danger } else { colors.text_secondary })
                                    .child(line),
                            ),
                    ),
            )
            .child(crate::smoke::tracked(
                "feed-topic-refresh",
                Button::secondary("feed-topic-refresh", if busy { "Refreshing" } else { "Refresh" })
                    .small()
                    .icon("refresh-cw")
                    .loading(busy)
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.refresh_topic(refresh_key.clone(), cx))),
            ))
            .into_any_element()
    }

    // --- For you ---

    fn render_post(&self, post: &FeedPost, now: f64, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let id = post.id.clone();

        if let Some(text) = self.hidden_now.get(&post.id) {
            let id = id.clone();
            return div()
                .flex()
                .items_center()
                .gap(px(space::MD))
                .pl(px(space::LG))
                .pr(px(space::SM))
                .py(px(space::SM))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .child(div().flex_1().min_w_0().subhead().text_color(colors.text_secondary).child(text.clone()))
                .child(
                    Button::ghost(SharedString::from(format!("undo-{id}")), "Undo")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| this.undo_hide(id.clone(), cx))),
                )
                .into_any_element();
        }

        let liked = self.like_overrides.get(&post.id).copied().unwrap_or(post.liked);
        let saved = self.save_overrides.get(&post.id).copied().unwrap_or(post.saved);
        let expanded = self.expanded.contains(&post.id);
        let (asking, adding) = self.ask_state(&post.id, cx);
        let view_post = {
            let id = id.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut gpui::App| {
                nav::push(Route::FeedPost(id.clone()), window, cx)
            }
        };

        // "…": why, less about the topic, not interested.
        let weak = cx.entity().downgrade();
        let menu_post = post.clone();
        let menu = MenuButton::new(SharedString::from(format!("menu-{id}")))
            .ghost()
            .small()
            .rounded(ButtonRounded::Size(px(radius::FULL)))
            .w(px(36.))
            .h(px(36.))
            .icon(Icon::empty().path("icons/ellipsis.svg").text_color(colors.text_secondary))
            .dropdown_menu_with_anchor(Corner::TopRight, move |menu, _, _| {
                let (why_view, less_view, not_view) = (weak.clone(), weak.clone(), weak.clone());
                let (why, post_a, post_b) = (menu_post.why.clone(), menu_post.clone(), menu_post.clone());
                menu.item(
                    PopupMenuItem::new("Why you're seeing this").icon(Icon::empty().path("icons/info.svg")).on_click(
                        move |_, _, cx| {
                            let why = why.clone();
                            why_view.update(cx, |this, cx| common::notify(this, why, cx)).ok();
                        },
                    ),
                )
                .item(
                    PopupMenuItem::new(format!("Less about {}", menu_post.topic_label))
                        .icon(Icon::empty().path("icons/bell-off.svg"))
                        .on_click(move |_, _, cx| {
                            less_view
                                .update(cx, |this, cx| this.hide(post_a.id.clone(), &post_a.topic_label, "less", cx))
                                .ok();
                        }),
                )
                .item(
                    PopupMenuItem::new("Not interested").icon(Icon::empty().path("icons/ban.svg")).on_click(
                        move |_, _, cx| {
                            not_view
                                .update(cx, |this, cx| {
                                    this.hide(post_b.id.clone(), &post_b.topic_label, "not_interested", cx)
                                })
                                .ok();
                        },
                    ),
                )
            });

        let head = div()
            .flex()
            .items_center()
            .gap(px(space::SM))
            .min_h(px(36.))
            .child(
                div()
                    .size(px(34.))
                    .flex_none()
                    .rounded(px(radius::FULL))
                    .bg(colors.surface_strong)
                    .flex()
                    .items_center()
                    .justify_center()
                    .footnote()
                    .bold()
                    .text_color(colors.text_secondary)
                    .child(
                        post.source_name
                            .trim()
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or("?".into()),
                    ),
            )
            .child(div().flex_1().min_w_0().footnote().semibold().truncate().child(post.source_name.clone()))
            .child(div().caption().text_color(colors.text_tertiary).child(format::age(post.created_at, now)))
            .child(menu);

        // Link-preview images are 1.91:1.
        let video = post.kind == "video";
        let cover = post.image_url.as_deref().and_then(|url| {
            common::post_cover(&id, url, self.column_width(), if video { 16. / 9. } else { 1.91 }, video, cx)
        });
        let has_cover = cover.is_some();
        let media = match cover {
            Some(cover) => {
                cover.cursor_pointer().hover(|style| style.opacity(0.94)).on_click(view_post.clone()).into_any_element()
            }
            // No picture, or it couldn't load: the title on a card.
            None => div()
                .id(SharedString::from(format!("media-{id}")))
                .flex()
                .flex_col()
                .gap(px(space::XS))
                .p(px(space::LG))
                .rounded(px(radius::LG))
                .bg(colors.surface)
                .cursor_pointer()
                .hover(|style| style.bg(colors.surface_strong))
                .on_click(view_post.clone())
                .child(div().title3().child(post.title.clone()))
                .into_any_element(),
        };

        let actions = {
            let (like_id, save_id, share_url, open_url, ask_post) =
                (id.clone(), id.clone(), post.url.clone(), post.url.clone(), post.clone());
            div()
                .flex()
                .items_center()
                .ml(px(-space::SM))
                .mr(px(-space::SM))
                .mt(px(-space::XS))
                .child(
                    IconButton::new(
                        SharedString::from(format!("like-{id}")),
                        if liked { "heart-filled" } else { "heart" },
                    )
                    .tooltip(if liked { "Unlike" } else { "Like" })
                    .when(liked, |this| this.color(colors.accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.like(like_id.clone(), !liked, cx))),
                )
                // Saved to Notes: not while Notes is off (pages.rs).
                .when(crate::pages::get(cx).notes, |this| {
                    this.child(
                        IconButton::new(
                            SharedString::from(format!("save-{id}")),
                            if saved { "bookmark-filled" } else { "bookmark" },
                        )
                        .tooltip(if saved { "Remove from Notes" } else { "Save to Notes" })
                        .when(saved, |this| this.color(colors.accent))
                        .on_click(cx.listener(move |this, _, _, cx| this.save(save_id.clone(), !saved, cx))),
                    )
                })
                // A post that's only an agent's words has no link to copy or open.
                .when(!post.url.is_empty(), |this| {
                    this.child(
                        IconButton::new(SharedString::from(format!("share-{id}")), "link")
                            .tooltip("Copy link")
                            .on_click(cx.listener(move |this, _, _, cx| common::share_post(this, &share_url, cx))),
                    )
                    .child(
                        IconButton::new(SharedString::from(format!("open-{id}")), "external-link")
                            .tooltip("Open in browser")
                            .on_click(move |_, _, cx| common::open_post(&open_url, cx)),
                    )
                })
                .child(div().flex_1())
                .child(crate::smoke::tracked(
                    format!("ask-{id}"),
                    // Filled while the chat beside the posts is about this post (a click closes it); while it's about
                    // others, a click adds this one.
                    (if asking { Button::primary } else { Button::secondary })(
                        SharedString::from(format!("ask-{id}")),
                        if adding {
                            "Add to chat".to_string()
                        } else {
                            common::FeedSettings::ask_label(self.settings())
                        },
                    )
                    .small()
                    .icon(if adding { "plus" } else { "message-circle" })
                    .on_click(cx.listener(move |this, _, window, cx| this.ask(ask_post.clone(), window, cx))),
                ))
        };

        let copy = div()
            .flex()
            .flex_col()
            .gap(px(space::XS))
            .when(has_cover, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("title-{id}")))
                        .headline()
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.8))
                        .on_click(view_post.clone())
                        .child(post.title.clone()),
                )
            })
            // Who shared it, with their note.
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(space::SM))
                    .child(div().pt(px(1.)).child(common::author_avatar(post, 18., cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .subhead()
                            .text_color(colors.text_secondary)
                            .child(div().semibold().text_color(colors.text).child(shared_by(post)))
                            .when(!post.note.trim().is_empty(), |this| this.child(post.note.clone())),
                    ),
            )
            .when_some(post.summary.clone().filter(|_| expanded), |this, summary| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(space::SM))
                        .pt(px(space::XS))
                        .child(div().callout().child(summary))
                        .children(post.points.iter().map(|point| {
                            div()
                                .flex()
                                .items_start()
                                .gap(px(space::SM))
                                .child(common::bullet(5., 9., colors.text_secondary))
                                .child(div().flex_1().min_w_0().subhead().child(point.clone()))
                        })),
                )
            })
            .when(post.summary.is_some(), |this| {
                let toggle_id = id.clone();
                let more = div().id(SharedString::from(format!("more-{id}")));
                // In a row of its own, so it (and its focus ring) is as wide as its words, not the column.
                this.child(
                    div().flex().child(crate::smoke::tracked(
                        format!("more-{id}"),
                        self.keyboard(&format!("more-{id}"), more, window, cx)
                            .flex()
                            .items_center()
                            .gap(px(space::XXS))
                            .py(px(space::XXS))
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.7))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_more(&toggle_id, cx)))
                            .child(div().footnote().semibold().text_color(colors.text_secondary).child(if expanded {
                                "Less"
                            } else {
                                "More"
                            }))
                            .child(ui::icon_sized(
                                if expanded { "chevron-up" } else { "chevron-down" },
                                16.,
                                colors.text_secondary,
                            )),
                    )),
                )
            })
            .when_some(post.event_starts_at.filter(|_| post.kind == "event"), |this, starts_at| {
                this.child(div().pt(px(space::XS)).child(common::event_row(
                    &format!("event-{id}"),
                    &post.id,
                    starts_at,
                    post.reminded,
                    false,
                    now,
                    cx,
                )))
            });

        div()
            .flex()
            .flex_col()
            .gap(px(space::SM + space::XXS))
            .child(head)
            .child(media)
            .child(actions)
            .child(copy)
            .into_any_element()
    }

    fn render_end(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let Some(Ok(feed)) = &self.feed else { return div().into_any_element() };
        let settings = feed.settings.as_ref();
        let count = self.listed_posts(&feed.posts).len();
        let topic_label = self.topic.as_ref().and_then(|key| self.topic_row(key)).map(|row| row.label.clone());
        let line = if self.glances_only {
            "That's every glance for now.".to_string()
        } else {
            common::feed_end_text(count, settings, topic_label.as_deref())
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(space::XS))
            .pt(px(space::XS))
            .child(div().footnote().text_color(colors.text_secondary).text_center().child(line))
            .into_any_element()
    }

    fn render_placeholder(&self, cx: &mut Context<Self>) -> AnyElement {
        let (feed, topics) = match (&self.feed, &self.topics) {
            (Some(Err(error)), _) | (_, Some(Err(error))) => {
                return ui::banner(error.clone(), ui::Tone::Danger, cx).into_any_element();
            }
            (Some(Ok(feed)), Some(Ok(topics))) => (feed, topics),
            _ => return feed_skeleton(self.column_width()).into_any_element(),
        };
        let settings = feed.settings.as_ref();
        let topic_label = self
            .topic
            .as_ref()
            .and_then(|key| topics.iter().find(|row| &row.topic_key == key))
            .map(|row| row.label.clone());
        let state = if self.glances_only {
            ui::empty_state(
                "chart-column-stacked",
                "No glances yet",
                Some(SharedString::from("Charts and summaries your agents post show here.")),
                None,
                cx,
            )
        } else if topics.is_empty() {
            ui::empty_state(
                "newspaper",
                "Pick a few topics",
                None,
                Some(
                    Button::primary("feed-add-topics", "Add topics")
                        .on_click(cx.listener(Self::open_settings))
                        .into_any_element(),
                ),
                cx,
            )
        } else if settings.is_some_and(|settings| settings.running) {
            ui::empty_state("newspaper", "Checking for new posts…", None, None, cx)
        } else {
            ui::empty_state(
                "newspaper",
                match &topic_label {
                    Some(label) => format!("Nothing on {label} yet"),
                    None => "No posts yet".into(),
                },
                settings
                    .and_then(|settings| settings.last_run_error.clone())
                    .or_else(|| settings.map(common::schedule_line))
                    .map(SharedString::from),
                // A topic has its own Refresh above; "Check now" looks at every topic.
                (settings.is_some() && self.topic.is_none()).then(|| {
                    Button::secondary("feed-check-now", "Check now")
                        .loading(self.refreshing)
                        .on_click(cx.listener(|this, _, _, cx| this.check_now(cx)))
                        .into_any_element()
                }),
                cx,
            )
        };
        div().min_h(px(320.)).flex().child(state).into_any_element()
    }
}

impl FeedScreen {
    /// "N new posts" over the top of the list while posts wait; a click brings them in at the top.
    fn render_arrived_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.arrived_count();
        if count == 0 {
            return None;
        }
        let colors = *cx.colors();
        let pill = div()
            .id("feed-new-posts")
            .flex()
            .items_center()
            .gap(px(space::XS))
            .h(px(32.))
            .px(px(space::MD))
            .rounded(px(radius::FULL))
            .bg(colors.action)
            .text_color(colors.on_action)
            .shadow_md()
            .footnote()
            .semibold()
            .cursor_pointer()
            .hover(|style| style.opacity(0.9))
            .active(|style| style.opacity(0.7))
            .on_click(cx.listener(|this, _, _, cx| this.show_arrived(cx)))
            .child(ui::icon_sized("arrow-up", 14., colors.on_action))
            .child(common::new_posts_label(count));
        Some(
            div()
                .absolute()
                .top(px(space::MD))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(crate::smoke::tracked("feed-new-posts", pill))
                .into_any_element(),
        )
    }
}

/// "Shared by Chef": who put the post in the Feed.
fn shared_by(post: &FeedPost) -> String {
    format!("Shared by {}", if post.author.trim().is_empty() { "an agent" } else { post.author.trim() })
}

impl Render for FeedScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_rows(cx);
        let colors = *cx.colors();
        let chat = self.chat.clone().filter(|_| self.chat_open);
        // Esc closes the Ask panel, as its × does (its draft kept).
        if let Some(chat) = chat.clone() {
            crate::escape::layer(window, cx, move |this, _, cx| {
                chat.update(cx, |chat, cx| chat.hidden(cx));
                this.chat_open = false;
                cx.notify();
            });
        }
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, index: usize, window, cx| this.render_row(index, window, cx)),
                        )
                        .size_full(),
                    )
                    .children(self.render_arrived_pill(cx))
                    .children(common::render_notice(&self.notice, cx)),
            )
            .when_some(chat, |this, chat| {
                this.child(
                    div()
                        .w(px(feed_chat::width(ui::content_width())))
                        .flex_none()
                        .h_full()
                        .border_l_1()
                        .border_color(colors.border)
                        .child(chat),
                )
            })
    }
}

/// Posts while the feed loads: the source, the cover, the actions and the title and summary.
fn feed_skeleton(width: f32) -> gpui::Div {
    div().flex().flex_col().gap(px(space::XXL)).children((0..2).map(move |_| {
        div()
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(space::SM))
                    .child(ui::skeleton(px(24.), 24., 12.))
                    .child(ui::skeleton(px(120.), 10., 5.))
                    .child(ui::skeleton(px(36.), 9., 4.5).secondary()),
            )
            .child(ui::skeleton(px(width), (width / 1.91).round(), radius::LG))
            .child(
                div().flex().gap(px(space::MD)).children((0..4).map(|_| ui::skeleton(px(20.), 20., 10.).secondary())),
            )
            .child(ui::skeleton(gpui::relative(0.82), 16., 7.))
            .child(ui::skeleton_text(&[0.96, 0.9, 0.55], 20.))
    }))
}

// --- Scripted checks (smoke.rs `FILO_OPEN=feed-drive`, `feed-chat`, `feed-refresh`; `dev` builds) ---

#[cfg(feature = "dev")]
impl FeedScreen {
    /// The covers of the posts listed now, in order: (post id, picture URL).
    pub fn drive_covers(&self) -> Vec<(String, String)> {
        let Some(Ok(feed)) = &self.feed else { return Vec::new() };
        self.listed_posts(&feed.posts)
            .into_iter()
            .filter_map(|post| Some((post.id.clone(), post.image_url.clone()?)))
            .collect()
    }

    pub fn drive_topics(&self) -> Vec<String> {
        match &self.topics {
            Some(Ok(topics)) => topics.iter().map(|topic| topic.topic_key.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Opens "More" on every post with a summary, or closes them all.
    pub fn drive_more(&mut self, open: bool, cx: &mut Context<Self>) {
        let posts: Vec<String> = match &self.feed {
            Some(Ok(feed)) => {
                feed.posts.iter().filter(|post| post.summary.is_some()).map(|post| post.id.clone()).collect()
            }
            _ => Vec::new(),
        };
        self.expanded.clear();
        if open {
            self.expanded.extend(posts);
        }
        cx.notify();
    }

    pub fn drive_topic(&mut self, topic: Option<String>, cx: &mut Context<Self>) {
        self.topic = topic;
        self.list.scroll_to(ListOffset::default());
        cx.notify();
    }

    /// Scrolls row `index` (0 is the header) to the top.
    pub fn drive_scroll_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let index = index.min(self.rows.len().saturating_sub(1));
        self.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.) });
        cx.notify();
    }

    /// Scrolls so row `index` is at the top, `offset` points scrolled into it.
    pub fn drive_scroll_into(&mut self, index: usize, offset: f32, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset {
            item_ix: index.min(self.rows.len().saturating_sub(1)),
            offset_in_item: px(offset),
        });
        cx.notify();
    }

    /// The row index of a post.
    pub fn drive_row_of(&self, post_id: &str) -> Option<usize> {
        self.rows.iter().position(|row| *row == Row::Post(post_id.to_string()))
    }

    /// Gives the keyboard focus to a chip or toggle (`topic-<key>`, `more-<post>`), as Tab would.
    pub fn drive_focus(&self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle(key, cx).focus(window);
        cx.notify();
    }

    pub fn drive_selected_topic(&self) -> Option<String> {
        self.topic.clone()
    }

    /// Where the list is: (first row on screen, points into it).
    pub fn drive_scroll_top(&self) -> (usize, f32) {
        let top = self.list.logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }

    pub fn drive_chat(&self) -> Option<Entity<FeedChat>> {
        self.chat.clone().filter(|_| self.chat_open)
    }

    /// Refresh presses answer with `answer` instead of asking the backend (None: ask it again).
    pub fn drive_mock_refresh(&mut self, answer: Option<Result<String, String>>) {
        self.mock_refresh = answer;
    }

    /// What the page's Refresh answers in the feed fixture (None: the Feed is up to date).
    pub fn drive_mock_check(&mut self, answer: Option<Result<String, String>>) {
        self.mock_check = answer;
    }

    /// Whether the page's Refresh is turning (an answer pending, its one turn, or a check it started running).
    pub fn drive_checking(&self) -> bool {
        self.refreshing || self.check_watch.is_some() || self.settings().is_some_and(|settings| settings.running)
    }

    /// The note at the bottom of the page.
    pub fn drive_notice(&self) -> Option<String> {
        self.notice.text()
    }

    /// A post's Ask button: its label and whether it's filled (the chat on screen is about it).
    pub fn drive_ask_button(&self, post_id: &str, cx: &Context<Self>) -> (String, bool) {
        let (asking, adding) = self.ask_state(post_id, cx);
        (if adding { "Add to chat".to_string() } else { common::FeedSettings::ask_label(self.settings()) }, asking)
    }

    /// Refresh asks sent so far (a press while one runs sends none).
    pub fn drive_refresh_asks(&self) -> usize {
        self.refresh_asks
    }

    /// The listed posts' ids, in order.
    pub fn drive_post_ids(&self) -> Vec<String> {
        match &self.feed {
            Some(Ok(feed)) => self.listed_posts(&feed.posts).into_iter().map(|post| post.id.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Stands in for `feed:mine` answering again with `feed` (a check that found new posts, or settings that changed).
    pub fn drive_feed(&mut self, feed: FeedMine, cx: &mut Context<Self>) {
        self.feed = Some(Ok(feed));
        self.note_arrivals();
        self.watch_check(cx);
        cx.notify();
    }

    pub fn drive_feed_now(&self) -> Option<FeedMine> {
        self.feed.as_ref().and_then(|feed| feed.as_ref().ok()).cloned()
    }

    /// Posts waiting behind the pill (the count it shows).
    pub fn drive_arrived(&self) -> usize {
        self.arrived_count()
    }

    pub fn drive_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Stands in for `feed:topicsMine` reporting a topic's Refresh state (the backend that has it isn't deployed
    /// where the check runs): running, then finished with `found` posts or `error`.
    pub fn drive_topic_state(
        &mut self,
        key: &str,
        refreshing: bool,
        found: Option<f64>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(Ok(mut topics)) = self.topics.clone() else { return };
        for topic in topics.iter_mut().filter(|topic| topic.topic_key == key) {
            topic.refreshing = refreshing;
            if !refreshing {
                topic.last_refreshed_at = Some(format::now_ms());
                topic.last_refresh_post_count = found;
                topic.last_refresh_error = error.clone();
            }
        }
        self.refresh_errors.remove(key);
        self.topics_pinned = true;
        self.set_topics(Ok(topics), cx);
    }

    /// The Refresh asked for a while ago: a new press asks again (as after REFRESH_PENDING).
    pub fn drive_forget_pending(&mut self) {
        self.refresh_pending.clear();
    }

    /// The feed fixture: made-up posts and topics stand in for the backend, which isn't asked again.
    pub fn drive_fixture(&mut self, feed: FeedMine, topics: Vec<FeedTopic>, cx: &mut Context<Self>) {
        self.feed_pinned = true;
        self.feed = Some(Ok(feed));
        self.note_arrivals();
        self.topics_pinned = true;
        self.set_topics(Ok(topics), cx);
    }

    /// The feed fixture's agents, for its Ask panel's picker.
    pub fn drive_agents(&mut self, agents: Vec<super::agents::AgentRow>) {
        self.fixture_agents = agents;
    }
}

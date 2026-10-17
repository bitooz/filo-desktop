// `FILO_OPEN=feed-fixture`: the Feed on made-up posts and topics, in a window of its own with no
// backend (covers come from `FILO_FIXTURE_IMAGES`, a local server; a Refresh is answered by a stand-in;
// Ask opens its panel but nothing is sent). For checking the Feed without touching real data.
//
// `FILO_FIXTURE_DRIVE=1` clicks and types through it with in-app AppKit events (smoke.rs `native_input`), logging each
// step and saving a screenshot of each in `FILO_FIXTURE_SHOTS=<folder>` (window_shot.rs), then quits. `FILO_OPEN=feed-settings-fixture`
// opens the Feed's settings page instead, on made-up settings, topics and agents (no change reaches a backend).
// `FILO_FIXTURE_THEME=dark` (or `light`) draws either theme whatever the app's appearance setting says.

use std::time::Duration;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use serde_json::json;

use super::feed::FeedScreen;
use super::feed_chat::FeedChat;
use super::feed_common::{FeedMine, FeedSettings, FeedTopic};
use super::feed_settings::FeedSettingsScreen;
use crate::smoke::{native_input, on_screen};
use crate::ui;

pub fn wanted() -> bool {
    std::env::var("FILO_OPEN").is_ok_and(|open| open == "feed-fixture" || open == "feed-settings-fixture")
}

pub fn open(cx: &mut App) {
    if let Ok(mode) = std::env::var("FILO_FIXTURE_THEME") {
        let mut prefs = crate::config::load_prefs();
        prefs.theme_mode = mode;
        crate::theme::apply(prefs, None, cx);
    }
    if std::env::var("FILO_OPEN").is_ok_and(|open| open == "feed-settings-fixture") {
        open_settings(cx);
        return;
    }
    let options = window_options(cx);
    cx.open_window(options, |window, cx| {
        let feed = cx.new(|cx| FeedScreen::new(window, cx));
        feed.update(cx, |feed, cx| {
            let (mine, topics) = data();
            feed.drive_fixture(mine, topics, cx);
            feed.drive_agents(agents_data());
            feed.drive_mock_refresh(Some(Ok("started".into())));
        });
        let fixture = cx.new(|_| FeedFixture { feed: feed.clone() });
        if std::env::var_os("FILO_FIXTURE_DRIVE").is_some() {
            drive(feed, window, cx);
        }
        cx.new(|cx| gpui_component::Root::new(fixture, window, cx))
    })
    .expect("open window");
}

struct FeedFixture<V: 'static> {
    feed: Entity<V>,
}

impl<V: Render> Render for FeedFixture<V> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::ActiveTheme as _;
        // The app shell tells the pages how wide they are (the sidebar's width is left out here).
        ui::set_content_width(f32::from(window.viewport_size().width));
        div().size_full().bg(cx.colors().background).text_color(cx.colors().text).child(self.feed.clone())
    }
}

fn window_options(cx: &mut App) -> WindowOptions {
    let (width, height) = crate::test_run::window_size((1100., 900.));
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Filo fixture".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        // Above other windows without taking the focus: the scripted check runs while the Mac is in use.
        kind: gpui::WindowKind::PopUp,
        focus: false,
        ..Default::default()
    }
}

/// Feed settings as `feed:settingsMine` answers them: Ask goes to the main agent (Filo), and the topics' routines
/// last ran five minutes ago.
fn settings_data(now: f64) -> FeedSettings {
    serde_json::from_value(json!({
        "pickMinute": 480, "nextRunAt": now + 25. * 60_000., "running": false, "lastRunAt": now - 5. * 60_000.,
        "failedRuns": 0,
        "intervalMinutes": 60, "askAgentId": "filo", "askAgentName": "Filo", "askAgentPicked": false,
    }))
    .expect("fixture settings")
}

/// The user's agents as `agents:list` answers them: Filo, the main agent, and two more.
fn agents_data() -> Vec<crate::screens::agents::AgentRow> {
    let agent = |id: &str, name: &str, picture: &str, background: Option<&str>, main: bool| json!({ "_id": id, "name": name, "picture": picture, "background": background, "instructions": "", "unseen": 0, "main": main });
    serde_json::from_value(json!([
        agent("filo", "Filo", "panda-waving", None, true),
        agent("chef", "Chef", "mint", Some("#FFD9C7"), false),
        agent("researcher", "Researcher", "owl-laptop", Some("#F97316"), false),
    ]))
    .expect("fixture agents")
}

fn open_settings(cx: &mut App) {
    let options = window_options(cx);
    cx.open_window(options, |window, cx| {
        let page = cx.new(|cx| FeedSettingsScreen::new(window, cx));
        page.update(cx, |page, cx| {
            let watched = |key: &str, label: &str, agent: (&str, &str, &str), every: Option<i64>, daily: Option<i64>| {
                json!({
                    "topicKey": key, "label": label, "routineId": format!("routine-{key}"), "agentId": agent.0, "agentName": agent.1,
                    "intervalMinutes": every, "pickMinute": daily,
                })
            };
            let filo = ("filo", "Filo", "panda-waving");
            let researcher = ("researcher", "Researcher", "owl-laptop");
            let topics: Vec<FeedTopic> = serde_json::from_value(json!([
                watched("agent memory", "Agent memory", researcher, Some(60), None),
                watched("cloudflare sandboxes", "Cloudflare sandboxes", researcher, Some(120), None),
                watched("one piece anime", "One Piece anime", filo, None, Some(480)),
                watched("jazz clubs in lisbon", "Jazz clubs in Lisbon", filo, None, Some(480)),
                { "topicKey": "rust gui", "label": "Rust GUI toolkits" },
            ]))
            .expect("fixture topics");
            page.drive_fixture(settings_data(crate::format::now_ms()), topics, agents_data(), cx);
        });
        let fixture = cx.new(|_| FeedFixture { feed: page.clone() });
        if std::env::var_os("FILO_FIXTURE_DRIVE").is_some() {
            drive_settings(page, window, cx);
        }
        cx.new(|cx| gpui_component::Root::new(fixture, window, cx))
    })
    .expect("open window");
}

fn drive_settings(page: Entity<FeedSettingsScreen>, _window: &mut Window, cx: &mut App) {
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let log = |line: String| eprintln!("feed-settings-fixture: {line}");
        let watch = |cx: &mut gpui::AsyncApp| {
            page.read_with(cx, |page, _| {
                page.drive_topics()
                    .iter()
                    .map(|topic| format!("{}: {}", topic.label, topic.watch_line()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        };
        wait(1500).await;
        log(format!("topics: {:?}", watch(cx)));
        shot("fs-01-topics-and-their-agents");
        // The topic nobody watches gets Chef, every 2 hours.
        page.update(cx, |page, cx| {
            page.drive_topic("rust gui", Some("chef"), None, cx);
            page.drive_topic("rust gui", None, Some(120), cx);
        })
        .ok();
        wait(700).await;
        log(format!("picked Chef for Rust GUI toolkits: {:?}", watch(cx).last()));
        shot("fs-02-topic-gets-an-agent");
        page.update(cx, |page, cx| page.drive_ask(Some("researcher"), cx)).ok();
        wait(700).await;
        log(format!("Ask answered by {:?}", page.read_with(cx, |page, _| page.drive_ask_name()).ok().flatten()));
        shot("fs-03-ask-answered-by-researcher");
        page.update(cx, |page, cx| page.drive_new(Some("chef"), Some(60), cx)).ok();
        wait(700).await;
        shot("fs-04-new-topic-pickers");
        log("done".into());
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|cx| cx.quit()).ok();
        }
    })
    .detach();
}

const HOUR: f64 = 60. * 60_000.;

fn data() -> (FeedMine, Vec<FeedTopic>) {
    let now = crate::format::now_ms();
    let images = std::env::var("FILO_FIXTURE_IMAGES").unwrap_or_else(|_| "http://127.0.0.1:8765".into());
    let summary = "A research group compared how ten agent memory systems store and find what they learned, across two \
        benchmark suites. Systems that write less and read more were cheaper over long sessions. The paper ends with \
        ten recommendations for builders.";
    let points = [
        "Ten systems, two benchmark suites",
        "Write-light designs cost less over time",
        "Ten recommendations for builders",
    ];
    let post = |id: &str, topic: &str, title: &str, source: &str, image: Option<&str>, hours: f64| {
        json!({
            "_id": id, "topicKey": topic, "topicLabel": topic, "kind": "article", "title": title,
            "note": "Picked for you: it compares the memory designs you asked about.", "why": "You follow this topic.",
            "url": format!("https://example.com/{id}"), "sourceName": source,
            "imageUrl": image.map(|name| format!("{images}/{name}")), "summary": summary, "points": points,
            "liked": false, "saved": false, "reminded": false, "createdAt": now - hours * HOUR,
            "author": "Researcher", "authorAgentId": "researcher", "authorPicture": "owl-laptop", "authorBackground": "#F97316",
        })
    };
    let mut mine: FeedMine = serde_json::from_value(json!({
        "posts": [
            post("p1", "agent memory", "Agent memory: how ten systems remember", "Example Research", Some("wide.png"), 1.),
            post("p2", "agent memory", "A persona graph that keeps claims traceable", "Example Papers", Some("square.png"), 2.),
            post("p3", "cloudflare sandboxes", "Sandboxes that keep their files between runs", "Example Blog", Some("missing.png"), 3.),
            post("p4", "cloudflare sandboxes", "Artifacts: versioned storage for agents", "Example Docs", None, 4.),
            post("p5", "one piece anime", "The next arc's first episode date", "Example News", Some("wide.png"), 5.),
            post("p6", "agent memory", "Why long context isn't memory", "Example Notes", Some("square.png"), 6.),
        ],
    }))
    .expect("fixture posts");
    mine.settings = Some(settings_data(now));
    let topics: Vec<FeedTopic> = serde_json::from_value(json!([
        { "topicKey": "agent memory", "label": "Agent memory" },
        { "topicKey": "cloudflare sandboxes", "label": "Cloudflare sandboxes" },
        { "topicKey": "one piece anime", "label": "One Piece anime" },
    ]))
    .expect("fixture topics");
    (mine, topics)
}

/// `<FILO_FIXTURE_SHOTS>/<name>.png`, when that's set.
fn shot(name: &str) {
    crate::window_shot::save_step(name);
}

fn drive(feed: Entity<FeedScreen>, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let wait = |ms: u64| executor.timer(Duration::from_millis(ms));
        let log = |line: String| eprintln!("feed-fixture: {line}");
        let window_width = |cx: &mut gpui::AsyncApp| -> f64 {
            handle.update(cx, |_, window, _| f64::from(f32::from(window.bounds().size.width))).unwrap_or(1100.)
        };
        let click = |id: &str, cx: &mut gpui::AsyncApp| -> bool {
            let Some(bounds) = on_screen(id) else {
                eprintln!("feed-fixture: {id} isn't on screen");
                return false;
            };
            let center = bounds.center();
            let height = handle.update(cx, |_, window, _| f32::from(window.viewport_size().height)).unwrap_or(0.);
            if f32::from(center.y) < 0. || f32::from(center.y) > height {
                eprintln!("feed-fixture: {id} is outside the window");
                return false;
            }
            native_input::click(window_width(cx), f64::from(f32::from(center.x)), f64::from(f32::from(center.y)))
        };
        let top = |cx: &mut gpui::AsyncApp| feed.read_with(cx, |feed, _| feed.drive_scroll_top()).unwrap_or_default();
        wait(2500).await;
        shot("fx-01-loaded");

        // A check finds two posts while p2 is being read: they wait behind the pill and the list stays put.
        if let Some(row) = feed.read_with(cx, |feed, _| feed.drive_row_of("p2")).ok().flatten() {
            feed.update(cx, |feed, cx| feed.drive_scroll_into(row, 120., cx)).ok();
            wait(700).await;
            let before = top(cx);
            let Some(mut mine) = feed.read_with(cx, |feed, _| feed.drive_feed_now()).ok().flatten() else { return };
            let original = mine.clone();
            let now = crate::format::now_ms();
            if let Some(settings) = &mut mine.settings {
                settings.running = true;
            }
            feed.update(cx, |feed, cx| feed.drive_feed(mine.clone(), cx)).ok();
            wait(500).await;
            shot("fx-01b-checking");
            let fresh: Vec<_> = [
                ("n1", "agent memory", "New: memory benchmarks for coding agents"),
                ("n2", "one piece anime", "Episode guide for the new arc"),
            ]
            .into_iter()
            .map(|(id, topic, title)| {
                let mut post = mine.posts[0].clone();
                (post.id, post.topic_key, post.topic_label, post.title, post.created_at) =
                    (id.into(), topic.into(), topic.into(), title.into(), now);
                post
            })
            .collect();
            mine.posts.splice(0..0, fresh);
            if let Some(settings) = &mut mine.settings {
                settings.running = false;
                settings.last_run_at = Some(now);
            }
            feed.update(cx, |feed, cx| feed.drive_feed(mine, cx)).ok();
            wait(800).await;
            let waiting = feed.read_with(cx, |feed, _| feed.drive_arrived()).unwrap_or_default();
            log(format!("arrived while reading: pill {waiting}, list top {before:?} -> {:?}", top(cx)));
            shot("fx-01c-new-posts-pill");
            let clicked = click("feed-new-posts", cx);
            wait(900).await;
            let ids = feed.read_with(cx, |feed, _| feed.drive_post_ids()).unwrap_or_default();
            log(format!(
                "clicked the pill ({clicked}): list top {:?}, first posts {:?}",
                top(cx),
                &ids[..ids.len().min(3)]
            ));
            shot("fx-01d-new-posts-shown");
            // Back to the fixture's own posts for the steps below, which expect p1 on top.
            feed.update(cx, |feed, cx| {
                feed.drive_feed(original, cx);
                feed.drive_scroll_row(0, cx);
            })
            .ok();
            wait(700).await;
        }
        log(format!(
            "covers: {:?}",
            feed.read_with(cx, |feed, cx| feed
                .drive_covers()
                .into_iter()
                .map(|(id, url)| (id, crate::pictures::state(&url, cx)))
                .collect::<Vec<_>>())
                .unwrap_or_default()
        ));

        // More on the first post, five times, the next post's cover in view.
        let open = |cx: &mut gpui::AsyncApp| feed.read_with(cx, |feed, _| feed.drive_expanded("p1")).unwrap_or(false);
        // Scrolled so p1's words and More are on screen with room for its summary, and p2's cover below.
        let row = feed.read_with(cx, |feed, _| feed.drive_row_of("p1")).ok().flatten().unwrap_or(1);
        feed.update(cx, |feed, cx| feed.drive_scroll_into(row, 420., cx)).ok();
        wait(900).await;
        for round in 1..=5 {
            click("more-p1", cx);
            wait(1200).await;
            let opened = open(cx);
            if round == 1 {
                shot("fx-02-more-open-next-cover-drawn");
            }
            click("more-p1", cx);
            wait(1200).await;
            log(format!("clicked More #{round}: open {opened}, then closed {}", !open(cx)));
        }
        feed.update(cx, |feed, cx| feed.drive_scroll_row(0, cx)).ok();
        wait(700).await;

        // Reading 80 points into a post, open its More: the list keeps the place.
        if let Some(row) = feed.read_with(cx, |feed, _| feed.drive_row_of("p2")).ok().flatten() {
            feed.update(cx, |feed, cx| feed.drive_scroll_into(row, 80., cx)).ok();
            wait(700).await;
            let before = top(cx);
            let clicked = click("more-p2", cx);
            wait(700).await;
            log(format!("anchor: clicked More ({clicked}) on p2 read 80 pt in: list top {before:?} -> {:?}", top(cx)));
            shot("fx-03-more-opened-while-reading-keeps-place");
            click("more-p2", cx);
            wait(500).await;
            feed.update(cx, |feed, cx| feed.drive_scroll_row(0, cx)).ok();
            wait(700).await;
        }

        // Ask Filo beside the posts, on the chat's own composer: opening sends nothing; Ask Filo on another post adds it
        // ("Add to chat"), on one in the chat closes it, the draft kept; a post not sent yet can be taken off; one added
        // after a message rides on the next one only.
        let posts = |chat: &Entity<FeedChat>, cx: &mut gpui::AsyncApp| {
            chat.read_with(cx, |chat, _| chat.drive_posts()).unwrap_or_default()
        };
        let draft = |chat: &Entity<FeedChat>, cx: &mut gpui::AsyncApp| {
            chat.read_with(cx, |chat, cx| chat.drive_draft(cx)).unwrap_or_default()
        };
        let unsent = |chat: &Entity<FeedChat>, cx: &mut gpui::AsyncApp| {
            chat.read_with(cx, |chat, cx| {
                (
                    chat.drive_unsent(),
                    chat.drive_placeholder(),
                    chat.drive_context(cx).map(|note| note.chars().take(90).collect::<String>()),
                )
            })
            .unwrap_or_default()
        };
        let button = |id: &str, cx: &mut gpui::AsyncApp| {
            feed.update(cx, |feed, cx| feed.drive_ask_button(id, cx)).unwrap_or(("?".to_string(), false))
        };
        let is_open =
            |cx: &mut gpui::AsyncApp| feed.read_with(cx, |feed, _| feed.drive_chat().is_some()).unwrap_or(false);
        click("ask-p1", cx);
        wait(900).await;
        let chat = feed.read_with(cx, |feed, _| feed.drive_chat()).ok().flatten();
        log(format!(
            "clicked Ask Filo on p1: panel open {}, buttons p1 {:?} p2 {:?}",
            chat.is_some(),
            button("p1", cx),
            button("p2", cx)
        ));
        shot("fx-04-ask-panel-open-composer");
        if let Some(chat) = chat.clone() {
            log(format!("p1 panel: posts {:?}, sending {:?}", posts(&chat, cx), unsent(&chat, cx)));
            click("composer-input", cx);
            for character in "What did they measure".chars() {
                native_input::key(window_width(cx), &character.to_string(), 0, 0);
            }
            wait(600).await;
            log(format!("typed into the chat composer: {:?}", draft(&chat, cx)));
            // The header's picker: the chat asks Chef instead, the posts and the draft going along.
            let asked = chat.read_with(cx, |chat, _| chat.drive_agent()).ok().flatten();
            handle.update(cx, |_, window, cx| chat.update(cx, |chat, cx| chat.drive_switch("chef", window, cx))).ok();
            wait(700).await;
            let now_asked = chat.read_with(cx, |chat, _| chat.drive_agent()).ok().flatten();
            log(format!(
                "switched the chat from {asked:?} to {now_asked:?}: posts {:?}, draft {:?}, sending {:?}",
                posts(&chat, cx),
                draft(&chat, cx),
                unsent(&chat, cx)
            ));
            shot("fx-04b-ask-switched-to-chef");
            let row = feed.read_with(cx, |feed, _| feed.drive_row_of("p2")).ok().flatten().unwrap_or(2);
            feed.update(cx, |feed, cx| feed.drive_scroll_into(row, 0., cx)).ok();
            wait(900).await;
            click("ask-p2", cx);
            wait(900).await;
            log(format!(
                "clicked Add to chat on p2: posts {:?}, draft {:?}, sending {:?}",
                posts(&chat, cx),
                draft(&chat, cx),
                unsent(&chat, cx)
            ));
            shot("fx-05-two-posts-draft-kept");
            click("ask-p2", cx);
            wait(700).await;
            log(format!("clicked Ask Filo on p2 again (in the chat): panel open {}", is_open(cx)));
            shot("fx-05b-toggled-closed");
            click("ask-p2", cx);
            wait(900).await;
            log(format!(
                "clicked Ask Filo on p2 with the panel closed: open {}, posts {:?} (no duplicate), draft {:?}",
                is_open(cx),
                posts(&chat, cx),
                draft(&chat, cx)
            ));
            let clicked = click("feed-chat-remove-p2", cx);
            wait(600).await;
            log(format!(
                "clicked Remove on p2 ({clicked}): posts {:?}, sending {:?}",
                posts(&chat, cx),
                unsent(&chat, cx)
            ));
            // A message carried p1 (stand-in: no backend); p3 added after it rides on the next message alone.
            handle.update(cx, |_, window, cx| chat.update(cx, |chat, cx| chat.drive_sent(window, cx))).ok();
            wait(300).await;
            log(format!("after a message carried p1: sending {:?}", unsent(&chat, cx)));
            let row = feed.read_with(cx, |feed, _| feed.drive_row_of("p3")).ok().flatten().unwrap_or(3);
            feed.update(cx, |feed, cx| feed.drive_scroll_into(row, 0., cx)).ok();
            wait(900).await;
            let label = button("p3", cx);
            click("ask-p3", cx);
            wait(900).await;
            log(format!(
                "clicked {label:?} on p3 after the first message: posts {:?}, sending {:?}, Remove on p1 {}, on p3 {}",
                posts(&chat, cx),
                unsent(&chat, cx),
                on_screen("feed-chat-remove-p1").is_some(),
                on_screen("feed-chat-remove-p3").is_some()
            ));
            shot("fx-05c-added-after-first-message");
            click("feed-chat-close", cx);
            wait(600).await;
            log(format!("clicked Close: panel open {}, draft kept {:?}", is_open(cx), draft(&chat, cx)));
            feed.update(cx, |feed, cx| feed.drive_scroll_row(0, cx)).ok();
            wait(700).await;
        }

        // A topic's Refresh, answered by a stand-in (no research runs).
        click("topic-0", cx);
        wait(800).await;
        click("feed-topic-refresh", cx);
        wait(250).await;
        shot("fx-06-refresh-asking");
        wait(900).await;
        feed.update(cx, |feed, cx| feed.drive_topic_state("agent memory", true, None, None, cx)).ok();
        wait(400).await;
        let asks = feed.read_with(cx, |feed, _| feed.drive_refresh_asks()).unwrap_or_default();
        click("feed-topic-refresh", cx);
        wait(400).await;
        log(format!(
            "Refresh asks before a click while running: {asks}, after: {}",
            feed.read_with(cx, |feed, _| feed.drive_refresh_asks()).unwrap_or_default()
        ));
        shot("fx-07-refresh-running");
        feed.update(cx, |feed, cx| {
            feed.drive_topic_state(
                "agent memory",
                false,
                Some(0.),
                Some("Search is not answering right now.".into()),
                cx,
            )
        })
        .ok();
        wait(600).await;
        shot("fx-08-refresh-failed-visible");

        // The keyboard: focus the For you chip as Tab would, Space; a post's More, Return.
        handle.update(cx, |_, window, cx| feed.update(cx, |feed, cx| feed.drive_focus("topic-all", window, cx))).ok();
        wait(400).await;
        native_input::key(window_width(cx), " ", 49, 0);
        wait(600).await;
        log(format!(
            "Space on the focused For you chip: topic {:?}",
            feed.read_with(cx, |feed, _| feed.drive_selected_topic()).ok().flatten()
        ));
        let before = open(cx);
        handle.update(cx, |_, window, cx| feed.update(cx, |feed, cx| feed.drive_focus("more-p1", window, cx))).ok();
        wait(600).await;
        native_input::key(window_width(cx), "\r", 36, 0);
        wait(900).await;
        log(format!("Return on the focused More: p1 open {before} -> {}", open(cx)));
        shot("fx-09-keyboard-focus-more-open");

        // The page's Refresh turns while it's asking (at least once), then says honestly what happened.
        feed.update(cx, |feed, cx| {
            feed.drive_topic(None, cx);
            feed.drive_scroll_row(0, cx);
        })
        .ok();
        wait(700).await;
        let checking = |cx: &mut gpui::AsyncApp| {
            feed.read_with(cx, |feed, _| (feed.drive_checking(), feed.drive_notice())).unwrap_or_default()
        };
        for (name, answer) in [("fresh", None), ("error", Some(Err("Couldn't reach Filo.".to_string())))] {
            feed.update(cx, |feed, _| feed.drive_mock_check(answer)).ok();
            let clicked = click("feed-refresh", cx);
            wait(250).await;
            let during = checking(cx);
            if name == "fresh" {
                shot("fx-12-refresh-turning");
            }
            wait(1100).await;
            log(format!("Refresh answered {name} ({clicked}): while asking {during:?}, after {:?}", checking(cx)));
            if name == "error" {
                shot("fx-13-refresh-error");
            }
        }
        feed.update(cx, |feed, _| feed.drive_mock_check(Some(Ok("started".into())))).ok();
        let Some(mut mine) = feed.read_with(cx, |feed, _| feed.drive_feed_now()).ok().flatten() else { return };
        let original = mine.clone();
        click("feed-refresh", cx);
        wait(1100).await;
        log(format!("Refresh started a check: {:?}", checking(cx)));
        if let Some(settings) = &mut mine.settings {
            settings.running = true;
        }
        feed.update(cx, |feed, cx| feed.drive_feed(mine.clone(), cx)).ok();
        wait(600).await;
        log(format!("check running: {:?}", checking(cx)));
        shot("fx-14-refresh-check-running");
        let mut found = mine.posts[0].clone();
        (found.id, found.title, found.created_at) = ("n3".into(), "Found by the check".into(), crate::format::now_ms());
        mine.posts.insert(0, found);
        if let Some(settings) = &mut mine.settings {
            settings.running = false;
            settings.last_run_at = Some(crate::format::now_ms());
        }
        feed.update(cx, |feed, cx| feed.drive_feed(mine, cx)).ok();
        wait(600).await;
        log(format!(
            "check over: {:?}, pill {}",
            checking(cx),
            feed.read_with(cx, |feed, _| feed.drive_arrived()).unwrap_or_default()
        ));
        shot("fx-15-refresh-check-done");
        feed.update(cx, |feed, cx| {
            feed.drive_mock_check(None);
            feed.drive_feed(original, cx);
        })
        .ok();
        wait(600).await;
        log("done".into());
        if std::env::var_os("FILO_DRIVE_STAY").is_none() {
            cx.update(|cx| cx.quit()).ok();
        }
    })
    .detach();
}

// /feed-settings (src/features/feed/feed-settings-screen.tsx and feed-topic-composer.tsx): who answers Ask on posts,
// the topics and the agent watching each one (how often, on its routine), and an input at the bottom to add a topic,
// with the agent and how often picked beside it, or rename the one being edited. The Feed is filled by agents only:
// a topic's agent looks for posts on its routine (local_backend/feed.rs), Refresh on the Feed runs those routines.

use gpui::{
    AnyElement, App, AppContext as _, Context, Corner, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, prelude::FluentBuilder as _, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button as MenuButton, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use serde_json::{Value, json};

use super::agent_picker;
use super::agents::AgentRow;
use super::feed_common::{self as common, FeedSettings, FeedTopic};
use crate::backend;
use crate::theme::{ActiveTheme as _, Typography as _, space};
use crate::ui::{self, IconButton, ListRow};

const COMPOSER_RADIUS: f32 = 22.;
/// How often an agent looks at a topic, as offered: every so many minutes (routines run every 30 minutes at most), or
/// once a day at 8:00.
const INTERVALS: [i64; 5] = [30, 60, 120, 6 * 60, 12 * 60];
const DAILY_AT: i64 = 8 * 60;

/// How often a topic's routine runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Often {
    Every(i64),
    Daily(i64),
}

impl Often {
    fn of(topic: &FeedTopic) -> Option<Often> {
        match (topic.interval_minutes, topic.pick_minute) {
            (Some(minutes), _) => Some(Often::Every(minutes as i64)),
            (None, Some(minute)) => Some(Often::Daily(minute as i64)),
            _ => None,
        }
    }

    fn label(self) -> String {
        match self {
            Often::Every(minutes) => super::routine_display::format_interval(minutes as f64),
            Often::Daily(minute) => format!("Daily at {}", common::pick_time_label(minute as f64)),
        }
    }

    /// The arguments `feed:addTopicMine` and `feed:updateTopicMine` take for it.
    fn args(self, mut args: Value) -> Value {
        match self {
            Often::Every(minutes) => args["intervalMinutes"] = json!(minutes as f64),
            Often::Daily(minute) => args["pickMinute"] = json!(minute as f64),
        }
        args
    }

    /// What the menu offers, `current` first when it's none of those.
    fn choices(current: Option<Often>) -> Vec<Often> {
        let mut choices: Vec<Often> =
            INTERVALS.iter().map(|minutes| Often::Every(*minutes)).chain([Often::Daily(DAILY_AT)]).collect();
        if let Some(current) = current.filter(|current| !choices.contains(current)) {
            choices.insert(0, current);
        }
        choices
    }
}

struct Editing {
    topic_key: String,
    label: String,
}

pub struct FeedSettingsScreen {
    topics: Option<Result<Vec<FeedTopic>, String>>,
    settings: Option<Result<Option<FeedSettings>, String>>,
    agents: Vec<AgentRow>,
    input: Entity<InputState>,
    editing: Option<Editing>,
    /// The topic being renamed was removed: clear the draft on the next render.
    clear_draft: bool,
    busy: bool,
    message: Option<String>,
    /// Who watches the next topic added (none: the main agent), and how often.
    new_agent: Option<String>,
    new_often: Often,
    /// The settings fixture (feed_fixture.rs): made-up settings, topics and agents stand in for the backend.
    #[cfg(feature = "dev")]
    pinned: bool,
    _subs: Vec<Task<()>>,
    _input_sub: Subscription,
}

impl FeedSettingsScreen {
    /// The settings fixture's stand-in (`drive_fixture`); never in the packaged app.
    fn pinned(&self) -> bool {
        #[cfg(feature = "dev")]
        return self.pinned;
        #[cfg(not(feature = "dev"))]
        false
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a topic"));
        let _input_sub = cx.subscribe_in(&input, window, |this, state, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                // A pasted line break would otherwise land in the topic; topics are at most 80 characters.
                let value = state.read(cx).value().to_string();
                let mut clean = value.replace(['\r', '\n'], " ");
                if clean.chars().count() > common::MAX_TOPIC_LENGTH {
                    clean = clean.chars().take(common::MAX_TOPIC_LENGTH).collect();
                }
                if clean != value {
                    state.update(cx, |state, cx| state.set_value(clean, window, cx));
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            _ => {}
        });
        let topics = backend::subscribe(
            cx,
            "feed:topicsMine",
            json!({}),
            |this: &mut Self, result: Result<Vec<FeedTopic>, String>, cx| {
                if this.pinned() {
                    return;
                }
                // A topic removed while it was being renamed ends the rename.
                if let (Some(editing), Ok(topics)) = (&this.editing, &result)
                    && !topics.iter().any(|topic| topic.topic_key == editing.topic_key)
                {
                    this.editing = None;
                    this.clear_draft = true;
                }
                this.topics = Some(result);
                cx.notify();
            },
        );
        let settings = backend::subscribe(cx, "feed:settingsMine", json!({}), |this: &mut Self, result, cx| {
            if this.pinned() {
                return;
            }
            this.settings = Some(result);
            cx.notify();
        });
        let agents = backend::subscribe(
            cx,
            "agents:list",
            json!({}),
            |this: &mut Self, result: Result<Vec<AgentRow>, String>, cx| {
                if this.pinned() {
                    return;
                }
                if let Ok(agents) = result {
                    // One picked for the next topic that's gone: back to the main agent.
                    if this.new_agent.as_ref().is_some_and(|id| !agents.iter().any(|agent| agent.id == *id)) {
                        this.new_agent = None;
                    }
                    this.agents = agents;
                    cx.notify();
                }
            },
        );
        FeedSettingsScreen {
            topics: None,
            settings: None,
            agents: Vec::new(),
            input,
            editing: None,
            clear_draft: false,
            busy: false,
            message: None,
            new_agent: None,
            new_often: Often::Daily(DAILY_AT),
            #[cfg(feature = "dev")]
            pinned: false,
            _subs: vec![topics, settings, agents],
            _input_sub,
        }
    }

    fn settings(&self) -> Option<&FeedSettings> {
        self.settings.as_ref().and_then(|settings| settings.as_ref().ok()).and_then(Option::as_ref)
    }

    fn agent(&self, id: Option<&str>) -> Option<&AgentRow> {
        id.and_then(|id| self.agents.iter().find(|agent| agent.id == id))
    }

    fn main_agent(&self) -> Option<&AgentRow> {
        self.agents.iter().find(|agent| agent.main)
    }

    fn reset_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder("Add a topic", window, cx);
        });
    }

    fn start_editing(&mut self, topic: &FeedTopic, window: &mut Window, cx: &mut Context<Self>) {
        self.message = None;
        self.editing = Some(Editing { topic_key: topic.topic_key.clone(), label: topic.label.clone() });
        let label = topic.label.clone();
        self.input.update(cx, |state, cx| {
            state.set_placeholder("Topic name", window, cx);
            state.set_value(label, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.reset_draft(window, cx);
        cx.notify();
    }

    /// Adds the topic typed, watched by the agent and as often as picked beside it (setting up its routine), or
    /// renames the one being edited.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.input.read(cx).value().trim().to_string();
        if label.is_empty() || self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        cx.notify();
        let renaming = self.editing.as_ref().map(|editing| editing.topic_key.clone());
        let (path, args) = match &renaming {
            Some(topic_key) => ("feed:renameTopicMine", json!({ "topicKey": topic_key, "label": label })),
            None => {
                let mut args = self.new_often.args(json!({ "label": label }));
                if let Some(agent) = &self.new_agent {
                    args["agentId"] = json!(agent);
                }
                ("feed:addTopicMine", args)
            }
        };
        backend::call_in(window, cx, path, args, move |this: &mut Self, result: Result<Value, String>, window, cx| {
            this.busy = false;
            match result {
                Ok(_) => {
                    if renaming.is_some() {
                        this.editing = None;
                    }
                    this.reset_draft(window, cx);
                }
                Err(error) => {
                    let fallback = if renaming.is_some() {
                        "Couldn't rename that. Try again."
                    } else {
                        "Couldn't add that. Try again."
                    };
                    this.message = Some(common::error_text(&error, fallback));
                }
            }
            cx.notify();
        });
    }

    fn remove(&mut self, topic_key: String, cx: &mut Context<Self>) {
        backend::call(
            cx,
            "feed:removeTopicMine",
            json!({ "topicKey": topic_key }),
            |this: &mut Self, result: Result<Value, String>, cx| {
                if result.is_err() {
                    this.message = Some("Couldn't remove that. Try again.".into());
                    cx.notify();
                }
            },
        );
    }

    /// Another agent for a topic, or another schedule (`feed:updateTopicMine`).
    fn update_topic(&mut self, topic_key: String, agent: Option<String>, often: Option<Often>, cx: &mut Context<Self>) {
        self.message = None;
        if self.pinned() {
            let agent_row = self.agent(agent.as_deref()).cloned();
            if let Some(Ok(topics)) = &mut self.topics
                && let Some(topic) = topics.iter_mut().find(|topic| topic.topic_key == topic_key)
            {
                if let Some(agent) = agent_row {
                    topic.routine_id.get_or_insert_with(|| "fixture".into());
                    (topic.agent_id, topic.agent_name) = (Some(agent.id), Some(agent.name));
                }
                match often {
                    Some(Often::Every(minutes)) => {
                        (topic.interval_minutes, topic.pick_minute) = (Some(minutes as f64), None)
                    }
                    Some(Often::Daily(minute)) => {
                        (topic.interval_minutes, topic.pick_minute) = (None, Some(minute as f64))
                    }
                    None => {}
                }
            }
            cx.notify();
            return;
        }
        let mut args = json!({ "topicKey": topic_key });
        if let Some(agent) = agent {
            args["agentId"] = json!(agent);
        }
        if let Some(often) = often {
            args = often.args(args);
        }
        backend::call(cx, "feed:updateTopicMine", args, |this: &mut Self, result: Result<Value, String>, cx| {
            if let Err(error) = result {
                this.message = Some(common::error_text(&error, "Couldn't change that topic. Try again."));
                cx.notify();
            }
        });
    }

    /// Who answers Ask on posts (none: the main agent).
    fn set_ask_agent(&mut self, agent: Option<String>, cx: &mut Context<Self>) {
        self.message = None;
        if self.pinned() {
            let row = self.agent(agent.as_deref()).or_else(|| self.main_agent()).cloned();
            if let Some(Ok(Some(settings))) = &mut self.settings {
                settings.ask_agent_picked = agent.is_some();
                settings.ask_agent_id = row.as_ref().map(|row| row.id.clone());
                settings.ask_agent_name = row.map(|row| row.name);
            }
            cx.notify();
            return;
        }
        backend::call(
            cx,
            "feed:updateSettingsMine",
            json!({ "askAgentId": agent }),
            |this: &mut Self, result: Result<Value, String>, cx| {
                if let Err(error) = result {
                    this.message = Some(common::error_text(&error, "Couldn't change who answers Ask."));
                    cx.notify();
                }
            },
        );
    }

    /// A menu of how often an agent looks at a topic, `current` checked.
    fn often_button(
        &self,
        id: impl Into<SharedString>,
        current: Option<Often>,
        pick: impl Fn(Often, &mut Window, &mut App) + Clone + 'static,
        cx: &App,
    ) -> AnyElement {
        let colors = *cx.colors();
        let label = current.map(Often::label).unwrap_or_else(|| "How often".into());
        MenuButton::new(id.into())
            .ghost()
            .small()
            .child(ui::icon_sized("clock-3", 14., colors.text_secondary))
            .child(div().footnote().text_color(colors.text_secondary).child(label))
            .dropdown_caret(true)
            .dropdown_menu_with_anchor(Corner::TopRight, move |mut menu, _, _| {
                for often in Often::choices(current) {
                    let pick = pick.clone();
                    menu = menu.item(
                        PopupMenuItem::new(often.label())
                            .checked(current == Some(often))
                            .on_click(move |_, window, cx| pick(often, window, cx)),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_ask(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = *cx.colors();
        let settings = self.settings()?;
        let picked = settings.ask_agent_id.as_deref().filter(|_| settings.ask_agent_picked).map(str::to_string);
        let shown = self.agent(settings.ask_agent_id.as_deref()).cloned();
        let name = settings.ask_agent_name.clone().unwrap_or_else(|| "Main agent".into());
        let weak = cx.entity().downgrade();
        let picker = agent_picker::button(
            "feed-ask-agent",
            self.agents.clone(),
            shown.as_ref(),
            &name,
            picked,
            true,
            move |agent, _, cx| {
                weak.update(cx, |this, cx| this.set_ask_agent(agent, cx)).ok();
            },
            cx,
        );
        let note = if settings.ask_agent_picked {
            format!("Ask on a post goes to {name}\u{2019}s conversation.")
        } else {
            format!("Ask on a post goes to your main agent, {name}. Pick another here.")
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(space::SM))
                .child(
                    div()
                        .px(px(space::SM))
                        .subhead()
                        .semibold()
                        .text_color(colors.text_secondary)
                        .child("Ask about posts"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(space::SM))
                        .child(div().px(px(space::SM)).callout().child("Answered by"))
                        .child(picker),
                )
                .child(div().px(px(space::SM)).footnote().text_color(colors.text_secondary).child(note))
                .into_any_element(),
        )
    }

    fn render_topics(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = *cx.colors();
        let topics = match &self.topics {
            None => return Some(ui::skeleton_rows(4, None, cx).into_any_element()),
            Some(Err(error)) => return Some(ui::banner(error.clone(), ui::Tone::Danger, cx).into_any_element()),
            Some(Ok(topics)) => topics,
        };
        if topics.is_empty() {
            return Some(
                div()
                    .px(px(space::SM))
                    .subhead()
                    .text_color(colors.text_secondary)
                    .child("Add what you\u{2019}d like to hear about, like \u{201c}jazz clubs in Lisbon\u{201d}, and pick the agent that watches it. It looks for new posts on its own, as often as you say.")
                    .into_any_element(),
            );
        }
        let rows = topics
            .iter()
            .enumerate()
            .map(|(index, topic)| {
                let selected = self.editing.as_ref().is_some_and(|editing| editing.topic_key == topic.topic_key);
                let (edit_topic, remove_key) = (topic.clone(), topic.topic_key.clone());
                let watcher = self.agent(topic.agent_id.as_deref()).cloned();
                let weak = cx.entity().downgrade();
                let key = topic.topic_key.clone();
                let agent = agent_picker::button(
                    SharedString::from(format!("feed-topic-agent-{index}")),
                    self.agents.clone(),
                    watcher.as_ref(),
                    topic.agent_name.as_deref().unwrap_or("Pick an agent"),
                    topic.agent_id.clone(),
                    false,
                    move |agent, _, cx| {
                        let key = key.clone();
                        weak.update(cx, |this, cx| this.update_topic(key, agent, None, cx)).ok();
                    },
                    cx,
                );
                let (weak, key) = (cx.entity().downgrade(), topic.topic_key.clone());
                let often = topic.routine_id.is_some().then(|| {
                    self.often_button(
                        SharedString::from(format!("feed-topic-often-{index}")),
                        Often::of(topic),
                        move |often, _, cx| {
                            let key = key.clone();
                            weak.update(cx, |this, cx| this.update_topic(key, None, Some(often), cx)).ok();
                        },
                        cx,
                    )
                });
                let trailing = div()
                    .id(SharedString::from(format!("feed-topic-controls-{index}")))
                    .flex()
                    .items_center()
                    .gap(px(space::XXS))
                    // The pickers' clicks aren't the row's (which renames).
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(agent)
                    .children(often)
                    .child(
                        IconButton::new(SharedString::from(format!("feed-topic-remove-{index}")), "x")
                            .small()
                            .color(colors.text_secondary)
                            .tooltip(format!("Remove {}", topic.label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove(remove_key.clone(), cx);
                            })),
                    );
                let subtitle = match (&topic.routine_id, &topic.agent_name) {
                    (Some(_), Some(name)) => format!("Watched by {name}, {}", topic.how_often()),
                    _ => topic.watch_line(),
                };
                div()
                    .when(selected, |this| this.bg(colors.selected_fill))
                    .child(
                        ListRow::new(SharedString::from(format!("feed-topic-{index}")), topic.label.clone())
                            .subtitle(subtitle)
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.start_editing(&edit_topic, window, cx)),
                            )
                            .trailing(trailing),
                    )
                    .into_any_element()
            })
            .collect();
        Some(
            ui::section(
                Some("Topics"),
                Some("Each topic is watched by an agent on a routine of its own. Click a topic to rename it.".into()),
                rows,
                cx,
            )
            .into_any_element(),
        )
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = *cx.colors();
        let can_submit = !self.input.read(cx).value().trim().is_empty() && !self.busy;
        let editing = self.editing.as_ref().map(|editing| editing.label.clone());
        // Who watches the next topic, and how often: beside the input while adding.
        let pickers = editing.is_none().then(|| {
            let shown = self.agent(self.new_agent.as_deref()).or_else(|| self.main_agent()).cloned();
            let weak = cx.entity().downgrade();
            let agent = agent_picker::button(
                "feed-new-agent",
                self.agents.clone(),
                shown.as_ref(),
                "Your main agent",
                self.new_agent.clone(),
                true,
                move |agent, _, cx| {
                    weak.update(cx, |this, cx| {
                        this.new_agent = agent;
                        cx.notify();
                    })
                    .ok();
                },
                cx,
            );
            let weak = cx.entity().downgrade();
            let often = self.often_button(
                "feed-new-often",
                Some(self.new_often),
                move |often, _, cx| {
                    weak.update(cx, |this, cx| {
                        this.new_often = often;
                        cx.notify();
                    })
                    .ok();
                },
                cx,
            );
            div()
                .px(px(space::XS))
                .flex()
                .items_center()
                .gap(px(space::XS))
                .child(div().footnote().text_color(colors.text_secondary).child("Watched by"))
                .child(agent)
                .child(often)
        });
        div()
            .flex_none()
            .border_t_1()
            .border_color(colors.border.opacity(0.5))
            .bg(colors.background)
            .child(
                ui::page_column()
                    .pt(px(space::SM))
                    .pb(px(space::MD))
                    .flex()
                    .flex_col()
                    .gap(px(space::SM))
                    .when_some(self.message.clone(), |this, message| {
                        this.child(div().px(px(space::SM)).footnote().text_color(colors.text_secondary).child(message))
                    })
                    .when_some(editing.clone(), |this, label| {
                        this.child(
                            div()
                                .px(px(space::SM))
                                .flex()
                                .items_center()
                                .gap(px(space::SM))
                                .child(ui::icon_sized("pencil", 14., colors.text_secondary))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .footnote()
                                        .text_color(colors.text_secondary)
                                        .truncate()
                                        .child(format!("Renaming \u{201c}{label}\u{201d}")),
                                )
                                .child(
                                    div()
                                        .id("feed-cancel-rename")
                                        .footnote()
                                        .semibold()
                                        .cursor_pointer()
                                        .hover(|style| style.opacity(0.7))
                                        .on_click(cx.listener(|this, _, window, cx| this.cancel_editing(window, cx)))
                                        .child("Cancel"),
                                ),
                        )
                    })
                    .children(pickers)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(space::XS))
                            .min_h(px(44.))
                            .p(px(3.))
                            .rounded(px(COMPOSER_RADIUS))
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.surface.opacity(0.86))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .pl(px(space::SM))
                                    .callout()
                                    .child(Input::new(&self.input).appearance(false)),
                            )
                            .child(
                                IconButton::new("feed-topic-submit", if editing.is_some() { "check" } else { "plus" })
                                    .primary()
                                    .small()
                                    .tooltip(if editing.is_some() { "Save topic" } else { "Add topic" })
                                    .disabled(!can_submit)
                                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for FeedSettingsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Esc cancels a topic's rename, as its Cancel does.
        if self.editing.is_some() {
            crate::escape::layer(window, cx, |this, window, cx| this.cancel_editing(window, cx));
        }
        if std::mem::take(&mut self.clear_draft) {
            self.reset_draft(window, cx);
        }
        let ask = self.render_ask(cx);
        let topics = self.render_topics(cx);
        let composer = self.render_composer(cx);

        ui::sub_page(
            ui::sub_page_header("feed-settings", "Your feed", vec![], cx),
            div().id("feed-settings-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                ui::page_column()
                    .pt(px(space::LG))
                    .pb(px(space::XL))
                    .flex()
                    .flex_col()
                    .gap(px(space::XL))
                    .when_some(self.settings.as_ref().and_then(|s| s.as_ref().err()).cloned(), |this, error| {
                        this.child(ui::banner(error, ui::Tone::Danger, cx))
                    })
                    .children(ask)
                    .children(topics),
            ),
            cx,
        )
        .child(composer)
    }
}

// --- The settings fixture (feed_fixture.rs `FILO_OPEN=feed-settings-fixture`) ---

impl FeedSettingsScreen {
    /// Made-up settings, topics and agents stand in for the backend, which isn't asked again; changes stay local.
    #[cfg(feature = "dev")]
    pub fn drive_fixture(
        &mut self,
        settings: FeedSettings,
        topics: Vec<FeedTopic>,
        agents: Vec<AgentRow>,
        cx: &mut Context<Self>,
    ) {
        self.pinned = true;
        self.settings = Some(Ok(Some(settings)));
        self.topics = Some(Ok(topics));
        self.agents = agents;
        cx.notify();
    }

    /// Another agent or schedule for a topic, as its pickers do.
    #[cfg(feature = "dev")]
    pub fn drive_topic(&mut self, topic_key: &str, agent: Option<&str>, every: Option<i64>, cx: &mut Context<Self>) {
        self.update_topic(topic_key.to_string(), agent.map(str::to_string), every.map(Often::Every), cx);
    }

    /// Who answers Ask, as its picker does (none: the main agent).
    #[cfg(feature = "dev")]
    pub fn drive_ask(&mut self, agent: Option<&str>, cx: &mut Context<Self>) {
        self.set_ask_agent(agent.map(str::to_string), cx);
    }

    /// Who watches the next topic and how often, as the pickers beside the input do.
    #[cfg(feature = "dev")]
    pub fn drive_new(&mut self, agent: Option<&str>, every: Option<i64>, cx: &mut Context<Self>) {
        self.new_agent = agent.map(str::to_string);
        self.new_often = every.map_or(Often::Daily(DAILY_AT), Often::Every);
        cx.notify();
    }

    #[cfg(feature = "dev")]
    pub fn drive_topics(&self) -> Vec<FeedTopic> {
        self.topics.as_ref().and_then(|topics| topics.as_ref().ok()).cloned().unwrap_or_default()
    }

    #[cfg(feature = "dev")]
    pub fn drive_ask_name(&self) -> Option<String> {
        self.settings().and_then(|settings| settings.ask_agent_name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn how_often_reads_as_offered() {
        assert_eq!(Often::Every(60).label(), "Every hour");
        assert_eq!(Often::Every(120).label(), "Every 2 hours");
        assert_eq!(Often::Every(30).label(), "Every 30 min");
        assert_eq!(Often::Daily(480).label(), "Daily at 8:00 AM");
        assert_eq!(Often::Every(30).args(json!({}))["intervalMinutes"], 30.0);
        assert_eq!(Often::Daily(600).args(json!({}))["pickMinute"], 600.0);
        // One that isn't offered shows first, so it can stay checked.
        assert_eq!(Often::choices(Some(Often::Daily(600)))[0], Often::Daily(600));
        assert_eq!(Often::choices(Some(Often::Every(60))).len(), 6);
    }
}

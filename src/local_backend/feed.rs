// The Feed on this Mac: the pages' `feed:*` functions, answered from Filo's database (crates/filo-local `feed`), the
// topics' routines, and Ask on posts.
//
// The Feed is filled by agents only. Each topic is watched by one of the user's agents on a routine of its own
// (crates/filo-local `schedule`, made when the topic is added, with the agent and how often picked then): its runs are
// turns in the agent's conversation that search the web for the topic and publish what they find with the MCP
// server's `publish_feed_post`, so each post says which agent shared it. Refresh, a topic's Refresh and Check now run
// those routines now (local_backend/scheduled.rs `run_job`); the schedule runs them on their own. Any conversation can
// publish a post too, and so can Claude Code or Codex outside Filo (`filo --feed-mcp`). Topics from before (when the
// Feed ran a check of its own) carry over to the main agent, on the schedule that check had (`feed_carry_over`).
//
// Ask on a post goes to an agent's conversation: the one picked in Feed settings (`askAgentId`), the main agent
// unless another is. The post page's Ask sends there and opens it; the Feed's own Ask panel shows that conversation
// beside the posts (its header can switch agents) and sends with `voiceSessions:sendMessage`, whose `contextNote`
// (the posts) goes to the engine with the message, not in the chat.
//
// A glance post (crates/filo-local `feed_mcp` `publish_feed_glance`) can offer choices: the user's pick goes back to the
// conversation that posted it as a message from them, and that agent's turn starts (`feed:chooseInGlance`).

use std::collections::HashMap;
use std::sync::Arc;

use filo_local::feed::{self, Post, Topic};
use filo_local::schedule::{self, Job, NewRoutine, Routine};
use filo_local::{notes, turn};
use serde_json::{Value, json};

use super::{Local, arg, in_namespace, not_here, now_ms, number, required};

/// The functions answered here, for the test that each is (the module answers its namespaces whole: `in_namespace`).
#[cfg(test)]
pub const FEED: &[&str] = &[
    "feed:mine",
    "feed:postMine",
    "feed:topicsMine",
    "feed:addTopicMine",
    "feed:renameTopicMine",
    "feed:updateTopicMine",
    "feed:removeTopicMine",
    "feed:setLikedMine",
    "feed:setSavedMine",
    "feed:hideMine",
    "feed:unhideMine",
    "feed:remindMine",
    "feed:askAboutPost",
    "feed:settingsMine",
    "feed:updateSettingsMine",
    "feed:refreshMine",
    "feed:refreshTopicMine",
    "feed:chooseInGlance",
];

/// An event's reminder comes this long before it starts, and no sooner than a minute from now.
const REMIND_BEFORE_MS: i64 = 30 * 60_000;
const REMIND_SOONEST_MS: i64 = 60_000;
/// Refresh with `ifStale` leaves a topic alone when its routine's last run ended this recently.
const FRESH_FOR_MS: i64 = 10 * 60_000;
/// A topic's routine is called this, so the Scheduled page says what it's for.
const ROUTINE_TITLE: &str = "Feed: ";

/// What a saved post becomes in Notes: its note, link, summary and points (a glance's text, for a glance).
fn note_body(post: &Post, glance_text: Option<&str>) -> String {
    let mut parts = vec![post.note.clone()];
    if let Some(text) = glance_text {
        parts.push(text.to_string());
    }
    if !post.url.is_empty() {
        parts.push(post.url.clone());
    }
    if let Some(summary) = &post.summary {
        parts.push(summary.clone());
    }
    if !post.points.is_empty() {
        parts.push(post.points.iter().map(|point| format!("- {point}")).collect::<Vec<_>>().join("\n"));
    }
    parts.into_iter().filter(|part| !part.trim().is_empty()).collect::<Vec<_>>().join("\n\n")
}

/// The post as a chat about it gets it (with what a glance shows, in words).
fn post_context(post: &Post, glance_text: Option<&str>) -> String {
    let mut lines = vec![
        format!("A post from the user's Filo Feed ({}, {}):", post.topic_label, post.source_name),
        format!("Title: {}", post.title),
    ];
    if !post.url.is_empty() {
        lines.push(format!("Link: {}", post.url));
    }
    lines.push(format!("Shared by: {}", post.author));
    if !post.note.is_empty() {
        lines.push(format!("Their note: {}", post.note));
    }
    if let Some(summary) = &post.summary {
        lines.push(format!("Summary: {summary}"));
    }
    lines.extend(post.points.iter().map(|point| format!("- {point}")));
    if let Some(text) = glance_text {
        lines.push(format!("It's a glance (a small visual) showing:\n{text}"));
    }
    lines.join("\n")
}

/// What a topic's routine does at each run, in its agent's conversation.
fn topic_instructions(label: &str) -> String {
    format!(
        "Look for new posts for the user's Feed in the Filo app on the topic \u{201c}{label}\u{201d}.\n\n\
1. Call list_feed_posts with topic \u{201c}{label}\u{201d} to see what's in the Feed already, so you don't repeat it.\n\
2. Search the web for recent, specific things on it: articles, videos, threads and upcoming events, from the last few days where you can. \
Each pick is a real page with a working link, not a search results page or a site's home page; open it when you can, to get it right.\n\
3. Publish up to 5 of the best with publish_feed_post, fewer or none when nothing is good enough: the title as the page has it, the link, \
the source's name, `topic` exactly \u{201c}{label}\u{201d}, a note of one or two sentences in your own words on why it's worth a look, `why` \
(what made you pick it), the kind, a summary and 3 to 5 points when you read the page, the page's preview image only if you saw its link \
on the page, and when an event starts.\n\
4. Then send one short line with send_message saying what you published, or that nothing new was good enough this time."
    )
}

/// How often a topic's routine runs, from `intervalMinutes` or `pickMinute` (a time of day): the routine's fields
/// (`days`, `minuteOfDay`, `schedule`). None when neither is given.
fn routine_timing(args: &Value) -> Result<Option<(Value, Value, Value)>, String> {
    if let Some(minutes) = number(args, "intervalMinutes") {
        let minutes = minutes.round() as i64;
        if !(schedule::MIN_INTERVAL_MINUTES..=24 * 60).contains(&minutes) {
            return Err("A topic is looked at every 30 minutes to every 24 hours.".into());
        }
        return Ok(Some((
            json!([0, 1, 2, 3, 4, 5, 6]),
            json!(0),
            json!({ "kind": "interval", "everyMinutes": minutes }),
        )));
    }
    if let Some(minute) = number(args, "pickMinute") {
        let minute = minute.round() as i64;
        if !(0..24 * 60).contains(&minute) {
            return Err("The time is a minute of the day, 0 to 1439.".into());
        }
        return Ok(Some((json!([0, 1, 2, 3, 4, 5, 6]), json!(minute), Value::Null)));
    }
    Ok(None)
}

/// The timing the Feed's own check had (feed_settings): what topics carried over to their routines get.
fn old_timing(settings: &feed::Settings) -> (Value, Value, Value) {
    let days = json!([0, 1, 2, 3, 4, 5, 6]);
    match settings.interval_minutes {
        Some(minutes) => (
            days,
            json!(0),
            json!({ "kind": "interval", "everyMinutes": minutes.clamp(schedule::MIN_INTERVAL_MINUTES, 24 * 60) }),
        ),
        None => (days, json!(settings.pick_minute), Value::Null),
    }
}

/// A topic with its routine and that routine's runs (newest first), as the pages show it.
struct Watched {
    topic: Topic,
    routine: Option<Routine>,
    runs: Vec<schedule::Run>,
}

impl Watched {
    fn refreshing(&self) -> bool {
        self.runs.first().is_some_and(|run| run.state == "queued" || run.state == "active")
    }

    fn last_finished(&self) -> Option<&schedule::Run> {
        self.runs.iter().find(|run| run.completed_at.is_some())
    }
}

impl Local {
    /// Answers the Feed's functions (`feed:`); `None` for any other path.
    pub(super) fn feed_call(self: &Arc<Self>, path: &str, args: &Value) -> Option<Result<Value, String>> {
        in_namespace(path, &["feed"]).then(|| self.feed_answer(path, args))
    }

    fn feed_answer(self: &Arc<Self>, path: &str, args: &Value) -> Result<Value, String> {
        let now = now_ms();
        let db = self.db()?;
        let post_id = || required(args, "postId");
        match path {
            "feed:mine" => Ok(
                json!({ "posts": self.posts_with_authors(feed::list(db, now)?), "settings": self.feed_settings(now)? }),
            ),
            "feed:postMine" => Ok(match feed::get(db, post_id()?)? {
                Some(post) => {
                    let mut detail = json!(post);
                    self.add_author(&mut detail);
                    detail
                }
                None => Value::Null,
            }),
            // There's no page reader here: a post has what its author wrote.
            "feed:topicsMine" => {
                Ok(Value::Array(self.watched()?.iter().map(|watched| self.topic_row(watched)).collect()))
            }
            "feed:addTopicMine" => self.add_topic(args, now),
            "feed:renameTopicMine" => {
                let (key, label) = (required(args, "topicKey")?, required(args, "label")?);
                if !feed::rename_topic(db, key, label)? {
                    return Err("That topic isn't in your Feed any more.".into());
                }
                // Its routine looks for it by its new name.
                if let Some(topic) = feed::topic(db, key)?
                    && let Some(routine) = topic.routine_id.as_deref()
                {
                    let patch = json!({ "title": format!("{ROUTINE_TITLE}{}", topic.label), "instructions": topic_instructions(&topic.label) });
                    schedule::update_routine(db, &chrono::Local, routine, &patch, None, now)?;
                }
                self.changed();
                Ok(Value::Null)
            }
            "feed:updateTopicMine" => self.update_topic(args, now),
            "feed:removeTopicMine" => {
                if let Some(routine) =
                    feed::remove_topic(db, required(args, "topicKey")?)?.and_then(|topic| topic.routine_id)
                {
                    schedule::delete_routine(db, &routine, None)?;
                }
                self.changed();
                Ok(Value::Null)
            }
            "feed:setLikedMine" => {
                let liked = args.get("liked").and_then(Value::as_bool).ok_or("`liked` is required")?;
                self.post_changed(feed::set_liked(db, post_id()?, liked)?)
            }
            "feed:setSavedMine" => {
                self.save_post(post_id()?, args.get("saved").and_then(Value::as_bool).ok_or("`saved` is required")?)
            }
            "feed:hideMine" => {
                let reason = required(args, "reason")?;
                self.post_changed(feed::set_hidden(db, post_id()?, Some(reason), now)?)
            }
            "feed:unhideMine" => self.post_changed(feed::set_hidden(db, post_id()?, None, now)?),
            "feed:remindMine" => self.remind_post(post_id()?, now),
            "feed:askAboutPost" => {
                let (session, agent, turn_id) =
                    self.ask_message(post_id()?, arg(args, "text"), arg(args, "agentId"))?;
                self.start_turn(self.agent_chats.clone(), session.clone(), turn_id);
                self.changed();
                Ok(json!({ "sessionId": session, "agentId": agent }))
            }
            "feed:settingsMine" => self.feed_settings(now),
            "feed:updateSettingsMine" => {
                // `askAgentId`: who answers Ask on posts; null goes back to the main agent.
                if let Some(agent) = args.get("askAgentId") {
                    let agent = agent.as_str().filter(|id| !id.is_empty());
                    if agent.is_some_and(|id| self.agent(id).is_none()) {
                        return Err("This agent no longer exists.".into());
                    }
                    feed::set_ask_agent(db, agent)?;
                    self.changed();
                }
                self.feed_settings(now)
            }
            "feed:refreshMine" => {
                let if_stale = args.get("ifStale").and_then(Value::as_bool).unwrap_or(false);
                let watched = self.watched()?;
                if watched.is_empty() {
                    return Ok(json!("no_topics"));
                }
                Ok(json!(self.refresh(watched, if_stale, now)))
            }
            "feed:refreshTopicMine" => {
                let key = required(args, "topicKey")?;
                let Some(watched) = self.watched()?.into_iter().find(|watched| watched.topic.topic_key == key) else {
                    return Ok(json!("not_found"));
                };
                Ok(json!(self.refresh(vec![watched], false, now)))
            }
            "feed:chooseInGlance" => {
                let (session, author, turn_id, store) =
                    self.glance_choice_message(post_id()?, required(args, "choice")?)?;
                self.start_turn(store, session.clone(), turn_id);
                self.changed();
                Ok(json!({ "sessionId": session, "author": author }))
            }
            _ => Err(not_here(path)),
        }
    }

    /// Who answers Ask on posts: the agent Feed settings picked while it's there, else the main agent.
    fn ask_agent(&self) -> Option<filo_local::app_data::Agent> {
        let picked = self.db().ok().and_then(|db| feed::settings(db).ok()).and_then(|settings| settings.ask_agent_id);
        picked.and_then(|id| self.agent(&id)).or_else(|| self.main_agent())
    }

    fn post_changed(&self, found: bool) -> Result<Value, String> {
        if !found {
            return Err("That post isn't in your Feed any more.".into());
        }
        self.changed();
        Ok(Value::Null)
    }

    // --- Who shared a post ---

    /// The posts, each with the agent that shared it when it's one of them (`add_author`).
    fn posts_with_authors(&self, posts: Vec<Post>) -> Value {
        let agents = self.agents();
        Value::Array(
            posts
                .into_iter()
                .map(|post| {
                    let mut row = json!(post);
                    author_of(&mut row, &agents);
                    row
                })
                .collect(),
        )
    }

    fn add_author(&self, row: &mut Value) {
        author_of(row, &self.agents());
    }

    // --- Topics and their routines ---

    /// The topics with their routines and runs, newest first.
    fn watched(&self) -> Result<Vec<Watched>, String> {
        let db = self.db()?;
        feed::topics(db)?
            .into_iter()
            .map(|topic| {
                let routine = match topic.routine_id.as_deref() {
                    Some(id) => schedule::routine(db, id, None)?,
                    None => None,
                };
                let runs = match &routine {
                    Some(routine) => schedule::runs(db, &routine.id)?,
                    None => Vec::new(),
                };
                Ok(Watched { topic, routine, runs })
            })
            .collect()
    }

    /// A topic as Feed settings and the Feed show it (`FeedTopic`): its agent, how often it's looked at, and what its
    /// routine's last run did.
    fn topic_row(&self, watched: &Watched) -> Value {
        let topic = &watched.topic;
        let mut row = json!({ "topicKey": topic.topic_key, "label": topic.label, "refreshing": watched.refreshing() });
        if let Some(routine) = &watched.routine {
            row["routineId"] = json!(routine.id);
            row["paused"] = json!(!routine.active());
            match &routine.schedule {
                Some(schedule::Repeat::Interval { every_minutes, .. }) => row["intervalMinutes"] = json!(every_minutes),
                Some(schedule::Repeat::Once { .. }) => {}
                None if routine.days.len() == 7 => row["pickMinute"] = json!(routine.minute_of_day),
                None => row["scheduleLabel"] = json!(schedule::describe_schedule(&chrono::Local, routine)),
            }
            // A routine of no agent's runs in its own chats, shown as the main agent's; Filo, the app, with no agents.
            let agent = match routine.agent_id.as_deref() {
                Some(id) => self.agent(id),
                None => self.main_agent(),
            };
            match agent {
                Some(agent) => {
                    if routine.agent_id.is_some() {
                        row["agentId"] = json!(agent.id);
                    }
                    row["agentName"] = json!(agent.name);
                }
                None => row["agentName"] = json!("Filo"),
            }
        }
        if let Some(run) = watched.last_finished() {
            let finished = run.completed_at.unwrap_or(run.created_at);
            row["lastRefreshedAt"] = json!(finished);
            if let Ok(found) =
                self.db().and_then(|db| feed::posts_between(db, &topic.topic_key, run.created_at, finished))
            {
                row["lastRefreshPostCount"] = json!(found);
            }
            if let Some(error) = &run.error {
                row["lastRefreshError"] = json!(error);
            }
        }
        row
    }

    /// The Feed's settings as the pages read them (`Settings`): who answers Ask, and what the topics' routines are up
    /// to as one Feed (running, when they last ran and how that went, how often the most frequent looks).
    fn feed_settings(&self, now: i64) -> Result<Value, String> {
        let db = self.db()?;
        let watched = self.watched()?;
        let routines: Vec<&Routine> = watched.iter().filter_map(|watched| watched.routine.as_ref()).collect();
        let interval = routines
            .iter()
            .filter_map(|routine| match routine.schedule {
                Some(schedule::Repeat::Interval { every_minutes, .. }) => Some(every_minutes),
                _ => None,
            })
            .min();
        let pick =
            routines.iter().filter(|routine| routine.schedule.is_none()).map(|routine| routine.minute_of_day).min();
        let next = routines.iter().filter(|routine| routine.active()).filter_map(|routine| routine.next_run_at).min();
        // The newest finished run of any topic.
        let last = watched
            .iter()
            .filter_map(|watched| watched.last_finished().map(|run| (watched, run)))
            .max_by_key(|(_, run)| run.completed_at);
        let ask = self.ask_agent();
        let stored = feed::settings(db)?;
        Ok(json!({
            "pickMinute": pick.unwrap_or(stored.pick_minute),
            "nextRunAt": next.unwrap_or(now + 24 * 60 * 60_000),
            "running": watched.iter().any(Watched::refreshing),
            "lastRunAt": last.and_then(|(_, run)| run.completed_at),
            "lastRunError": last.and_then(|(_, run)| run.error.clone()),
            "failedRuns": usize::from(last.is_some_and(|(_, run)| run.error.is_some())),
            "intervalMinutes": interval,
            "askAgentId": ask.as_ref().map(|agent| agent.id.clone()),
            "askAgentName": ask.as_ref().map(|agent| agent.name.clone()),
            // Picked in Feed settings, rather than the main agent by default.
            "askAgentPicked": stored.ask_agent_id.is_some_and(|id| self.agent(&id).is_some()),
        }))
    }

    /// `feed:addTopicMine`: follows a topic and sets up its routine, watched by `agentId` (the main agent by default)
    /// every `intervalMinutes` or each day at `pickMinute` (8:00 by default).
    fn add_topic(&self, args: &Value, now: i64) -> Result<Value, String> {
        let db = self.db()?;
        let agent = match arg(args, "agentId") {
            Some(id) => self.agent(id).ok_or("This agent no longer exists.")?,
            None => self.main_agent().ok_or("Add an agent on the Agents tab to watch this topic.")?,
        };
        let timing = routine_timing(args)?
            .unwrap_or_else(|| (json!([0, 1, 2, 3, 4, 5, 6]), json!(feed::DEFAULT_PICK_MINUTE), Value::Null));
        let key = feed::add_topic(db, required(args, "label")?, now)?;
        let label = feed::topic(db, &key)?.map(|topic| topic.label).unwrap_or_default();
        match self.make_topic_routine(&key, &label, Some(&agent.id), timing, now) {
            Ok(_) => {
                self.changed();
                Ok(json!({ "topicKey": key }))
            }
            Err(error) => {
                feed::remove_topic(db, &key)?;
                Err(error)
            }
        }
    }

    /// The routine that watches a topic, made and kept with it.
    fn make_topic_routine(
        &self,
        key: &str,
        label: &str,
        agent: Option<&str>,
        (days, minute_of_day, timing): (Value, Value, Value),
        now: i64,
    ) -> Result<Routine, String> {
        let db = self.db()?;
        let new = NewRoutine {
            title: format!("{ROUTINE_TITLE}{label}"),
            instructions: topic_instructions(label),
            days,
            minute_of_day,
            schedule: timing,
            agent_id: agent.map(str::to_string),
        };
        let routine = schedule::create_routine(db, &chrono::Local, new, now)?;
        feed::set_topic_routine(db, key, Some(&routine.id))?;
        Ok(routine)
    }

    /// `feed:updateTopicMine`: another agent (`agentId`) or how often (`intervalMinutes`, `pickMinute`) for a topic. A
    /// topic whose routine is gone (deleted on the Scheduled page) gets a new one.
    fn update_topic(&self, args: &Value, now: i64) -> Result<Value, String> {
        let db = self.db()?;
        let key = required(args, "topicKey")?;
        let topic = feed::topic(db, key)?.ok_or("That topic isn't in your Feed any more.")?;
        let agent = arg(args, "agentId").map(|id| self.agent(id).ok_or("This agent no longer exists.")).transpose()?;
        let timing = routine_timing(args)?;
        let routine = match topic.routine_id.as_deref() {
            Some(id) => schedule::routine(db, id, None)?,
            None => None,
        };
        match routine {
            Some(routine) => {
                if let Some(agent) = &agent {
                    schedule::set_routine_agent(db, &routine.id, Some(&agent.id), now)?;
                }
                if let Some((days, minute_of_day, timing)) = timing {
                    let patch =
                        json!({ "days": days, "minuteOfDay": minute_of_day, "schedule": timing, "status": "active" });
                    schedule::update_routine(db, &chrono::Local, &routine.id, &patch, None, now)?;
                }
            }
            None => {
                let agent =
                    agent.or_else(|| self.main_agent()).ok_or("Add an agent on the Agents tab to watch this topic.")?;
                let timing = timing
                    .unwrap_or_else(|| (json!([0, 1, 2, 3, 4, 5, 6]), json!(feed::DEFAULT_PICK_MINUTE), Value::Null));
                self.make_topic_routine(key, &topic.label, Some(&agent.id), timing, now)?;
            }
        }
        self.changed();
        Ok(Value::Null)
    }

    /// Runs the topics' routines now (Refresh): each that isn't running already, nor (`if_stale`) ran a moment ago.
    /// What the Feed says about it: started, running, fresh, or no_agent when no topic has one.
    fn refresh(self: &Arc<Self>, watched: Vec<Watched>, if_stale: bool, now: i64) -> &'static str {
        let Ok(db) = self.db() else { return "failed" };
        let (mut started, mut running, mut fresh) = (0, 0, 0);
        for watched in &watched {
            let Some(routine) = &watched.routine else { continue };
            if watched.refreshing() {
                running += 1;
                continue;
            }
            if if_stale
                && watched.last_finished().and_then(|run| run.completed_at).is_some_and(|at| now - at < FRESH_FOR_MS)
            {
                fresh += 1;
                continue;
            }
            match schedule::start_run_now(db, &routine.id, None, now) {
                Ok(Some((routine, run))) => {
                    started += 1;
                    self.run_job(Job::Routine { routine, run }, false);
                }
                Ok(None) => {}
                Err(_) => running += 1,
            }
        }
        self.changed();
        match (started, running, fresh) {
            (1.., _, _) => "started",
            (0, 1.., _) => "running",
            (0, 0, 1..) => "fresh",
            _ => "no_agent",
        }
    }

    /// Topics from when the Feed ran a check of its own have no routine: each gets one, watched by the main agent on
    /// the schedule that check had. Once, from the app (`start_schedule`), while the Feed is on; turned off, it waits
    /// until the Feed is turned on again (`set_page`), so no routine runs for a page the user turned off.
    pub(super) fn feed_carry_over(&self) {
        let Ok(db) = self.db() else { return };
        if !filo_local::pages::get(db).feed {
            return;
        }
        let (Ok(topics), Ok(settings)) = (feed::topics(db), feed::settings(db)) else { return };
        let Some(main) = self.main_agent() else { return };
        let now = now_ms();
        let mut carried = 0;
        for topic in topics.iter().filter(|topic| topic.routine_id.is_none()) {
            match self.make_topic_routine(&topic.topic_key, &topic.label, Some(&main.id), old_timing(&settings), now) {
                Ok(_) => carried += 1,
                Err(error) => eprintln!("filo: the Feed topic “{}” couldn't get its routine: {error}", topic.label),
            }
        }
        if carried > 0 {
            eprintln!("feed: {carried} topics carried over to {}", main.name);
            self.changed();
        }
    }

    // --- Saves, reminders and Ask ---

    /// Saving makes a note tagged `feed` from the post; unsaving moves that note to the trash.
    fn save_post(&self, post_id: &str, saved: bool) -> Result<Value, String> {
        let db = self.db()?;
        let post = feed::get(db, post_id)?.ok_or("That post isn't in your Feed any more.")?;
        let note_id = match (saved, post.note_id.clone()) {
            (true, Some(note)) if notes::get(db, &note)?.is_some() => Some(note),
            (true, _) => {
                let glance = feed::post_glance(db, post_id)?;
                let body = note_body(&post, glance.as_ref().map(|glance| glance.text.as_str()));
                let fields =
                    notes::Fields { title: post.title.clone(), body, tags: Some(vec!["feed".into()]), pinned: None };
                Some(notes::create(db, &fields)?)
            }
            (false, note) => {
                if let Some(note) = note {
                    notes::delete(db, &note)?;
                }
                None
            }
        };
        feed::set_note(db, post_id, note_id.as_deref())?;
        self.changed();
        Ok(json!({ "noteId": note_id }))
    }

    /// A reminder 30 minutes before an event post starts.
    fn remind_post(&self, post_id: &str, now: i64) -> Result<Value, String> {
        let db = self.db()?;
        let post = feed::get(db, post_id)?.ok_or("That post isn't in your Feed any more.")?;
        let starts = post.event_starts_at.filter(|_| post.kind == "event").ok_or("Only events can have a reminder.")?;
        if starts <= now {
            return Err("That event has already started.".into());
        }
        let due_at = (starts - REMIND_BEFORE_MS).max(now + REMIND_SOONEST_MS).min(starts);
        let clock = chrono::TimeZone::timestamp_millis_opt(&chrono::Local, starts)
            .earliest()
            .map(|at| at.format("%-I:%M %p").to_string())
            .unwrap_or_default();
        let new = schedule::NewReminder {
            title: post.title.chars().take(120).collect(),
            note: Some(format!("Starts at {clock}.")),
            due_at,
            recurrence: None,
            delivery: None,
            agent_id: None,
            chat_id: None,
        };
        let reminder = schedule::create_reminder(db, new, now)?;
        feed::set_reminder(db, post_id, Some(&reminder.id))?;
        self.changed();
        Ok(json!({ "reminderId": reminder.id, "dueAt": reminder.due_at }))
    }

    /// The post page's Ask: a message about the post in the conversation of the agent that answers Ask (`agentId`, else
    /// the one Feed settings picked, else the main agent), for its turn to start. The conversation, the agent and the turn.
    fn ask_message(
        &self,
        post_id: &str,
        text: Option<&str>,
        agent: Option<&str>,
    ) -> Result<(String, String, String), String> {
        let post = feed::get(self.db()?, post_id)?.ok_or("That post isn't in your Feed any more.")?;
        let agent = match agent {
            Some(id) => self.agent(id).ok_or("This agent no longer exists.")?,
            None => self.ask_agent().ok_or("Add an agent on the Agents tab to ask about this.")?,
        };
        let session =
            self.open_agent_chat(&agent.id)?.as_str().map(str::to_string).ok_or("Couldn't open the conversation.")?;
        let message = text
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Tell me more about \u{201c}{}\u{201d}.", post.title));
        let turn_id = uuid::Uuid::new_v4().to_string();
        let store = self.agent_chats.clone();
        store
            .add_user_message(&session, &message, &turn_id)
            .map_err(|error| format!("Couldn't save the message: {error}"))?;
        let glance = feed::post_glance(self.db()?, post_id)?;
        let context = post_context(&post, glance.as_ref().map(|glance| glance.text.as_str()));
        turn::save_context(&store, &session, &turn_id, &context)
            .map_err(|error| format!("Couldn't save the post for the chat: {error}"))?;
        Ok((session, agent.id, turn_id))
    }

    /// A pick in a glance post's choices: a message from the user in the conversation that posted it (or, for one from
    /// outside Filo, the conversation of the agent by its name), with the post for the engine, for its turn to start.
    /// The conversation, who posted it, the turn and the store it's in. The pick is kept on the post.
    fn glance_choice_message(
        &self,
        post_id: &str,
        choice: &str,
    ) -> Result<(String, String, String, filo_local::Store), String> {
        let db = self.db()?;
        let post = feed::get(db, post_id)?.ok_or("That post isn't in your Feed any more.")?;
        let choice: String =
            choice.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(feed::MAX_CHOICE).collect();
        if post.kind != feed::GLANCE_KIND || choice.is_empty() {
            return Err("There's nothing to pick in that post.".into());
        }
        let posted_in =
            post.chat_id.as_deref().and_then(|chat| self.store_of(chat).map(|store| (chat.to_string(), store.clone())));
        let (session, store) = match posted_in {
            Some(found) => found,
            None => {
                let agent = self
                    .agents()
                    .into_iter()
                    .find(|agent| agent.name == post.author)
                    .ok_or("The conversation this came from isn't there any more.")?;
                let session = self
                    .open_agent_chat(&agent.id)?
                    .as_str()
                    .map(str::to_string)
                    .ok_or("Couldn't open the conversation.")?;
                (session, self.agent_chats.clone())
            }
        };
        let message =
            format!("I picked \u{201c}{choice}\u{201d} on your glance \u{201c}{}\u{201d} in the Feed.", post.title);
        let turn_id = uuid::Uuid::new_v4().to_string();
        store
            .add_user_message(&session, &message, &turn_id)
            .map_err(|error| format!("Couldn't send your pick: {error}"))?;
        let glance = feed::post_glance(db, post_id)?;
        let context = format!(
            "{}\nThe user picked \u{201c}{choice}\u{201d} in it (post id {}). Act on it, and keep the glance current with update_feed_glance if it should show the answer.",
            post_context(&post, glance.as_ref().map(|glance| glance.text.as_str())),
            post.id
        );
        turn::save_context(&store, &session, &turn_id, &context)
            .map_err(|error| format!("Couldn't send your pick: {error}"))?;
        feed::set_glance_choice(db, post_id, Some(&choice))?;
        Ok((session, post.author.clone(), turn_id, store))
    }
}

/// The agent that shared a post (`authorAgentId`, `authorPicture`, `authorBackground`): the one whose conversation
/// it came from, else the agent by its name.
fn author_of(row: &mut Value, agents: &[filo_local::app_data::Agent]) {
    let chat = row["chatId"].as_str().map(str::to_string);
    let name = row["author"].as_str().unwrap_or_default().to_string();
    let by_chat: HashMap<&str, &filo_local::app_data::Agent> =
        agents.iter().filter_map(|agent| Some((agent.session_id.as_deref()?, agent))).collect();
    let agent = chat
        .as_deref()
        .and_then(|chat| by_chat.get(chat).copied())
        .or_else(|| agents.iter().find(|agent| agent.name == name));
    if let Some(agent) = agent {
        row["authorAgentId"] = json!(agent.id);
        row["authorPicture"] = json!(agent.picture);
        row["authorBackground"] = json!(agent.background);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> crate::local_backend::test_support::TempLocal {
        crate::local_backend::test_support::temp("feed")
    }

    fn publish(local: &Local, title: &str, url: &str, kind: &str, starts: Option<i64>) -> String {
        let new = feed::NewPost {
            title: title.into(),
            url: url.into(),
            note: "Look.".into(),
            kind: Some(kind.into()),
            event_starts_at: starts,
            ..Default::default()
        };
        let origin = feed::Origin { author: "Scout".into(), chat_id: None };
        feed::publish(local.db().unwrap(), &new, &origin, now_ms()).unwrap().id
    }

    #[test]
    fn the_pages_read_and_change_the_feed() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let mine = call("feed:mine", json!({})).unwrap();
        assert_eq!(mine["posts"], json!([]));
        assert_eq!(
            (mine["settings"]["running"].as_bool(), mine["settings"]["pickMinute"].as_i64()),
            (Some(false), Some(480))
        );
        assert_eq!(call("feed:refreshMine", json!({})).unwrap(), "no_topics");

        let key = call("feed:addTopicMine", json!({ "label": "Lisbon jazz" })).unwrap()["topicKey"]
            .as_str()
            .unwrap()
            .to_string();
        call("feed:renameTopicMine", json!({ "topicKey": key, "label": "Jazz in Lisbon" })).unwrap();
        assert_eq!(call("feed:topicsMine", json!({})).unwrap()[0]["label"], "Jazz in Lisbon");
        assert!(call("feed:renameTopicMine", json!({ "topicKey": "nope", "label": "x" })).is_err());

        let id = publish(&local, "Fado night", "https://example.com/fado", "event", Some(now_ms() + 3 * 3_600_000));
        let post = &call("feed:mine", json!({})).unwrap()["posts"][0];
        assert_eq!(
            (post["_id"].as_str(), post["title"].as_str(), post["sourceName"].as_str()),
            (Some(id.as_str()), Some("Fado night"), Some("example.com"))
        );
        assert_eq!(call("feed:postMine", json!({ "postId": id })).unwrap()["title"], "Fado night");
        assert_eq!(call("feed:postMine", json!({ "postId": "gone" })).unwrap(), Value::Null);

        call("feed:setLikedMine", json!({ "postId": id, "liked": true })).unwrap();
        let saved = call("feed:setSavedMine", json!({ "postId": id, "saved": true })).unwrap();
        let note = saved["noteId"].as_str().unwrap().to_string();
        let made = notes::get(local.db().unwrap(), &note).unwrap().unwrap();
        assert_eq!((made.title.as_str(), made.tags.as_slice()), ("Fado night", ["feed".to_string()].as_slice()));
        assert!(made.body.contains("https://example.com/fado"));
        let post = &call("feed:mine", json!({})).unwrap()["posts"][0];
        assert_eq!((post["liked"].as_bool(), post["saved"].as_bool()), (Some(true), Some(true)));
        assert_eq!(call("feed:setSavedMine", json!({ "postId": id, "saved": false })).unwrap()["noteId"], Value::Null);
        assert_eq!(notes::get(local.db().unwrap(), &note).unwrap(), None);

        let reminded = call("feed:remindMine", json!({ "postId": id })).unwrap();
        assert!(reminded["dueAt"].as_i64().unwrap() > now_ms());
        assert_eq!(call("feed:mine", json!({})).unwrap()["posts"][0]["reminded"], true);
        assert_eq!(call("reminders:myReminders", json!({})).unwrap()[0]["title"], "Fado night");
        let article = publish(&local, "Not an event", "https://example.com/a", "article", None);
        assert!(call("feed:remindMine", json!({ "postId": article })).unwrap_err().contains("Only events"));

        call("feed:hideMine", json!({ "postId": article, "reason": "not_interested" })).unwrap();
        let posts = call("feed:mine", json!({})).unwrap()["posts"].clone();
        assert_eq!(
            posts.as_array().unwrap().iter().find(|post| post["_id"] == article.as_str()).unwrap()["hidden"],
            "not_interested"
        );
        call("feed:unhideMine", json!({ "postId": article })).unwrap();
        assert!(call("feed:hideMine", json!({ "postId": "gone", "reason": "less" })).is_err());

        call("feed:removeTopicMine", json!({ "topicKey": key })).unwrap();
        assert_eq!(call("feed:topicsMine", json!({})).unwrap(), json!([]));
        assert_eq!(call("feed:refreshTopicMine", json!({ "topicKey": key })).unwrap(), "not_found");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn each_topic_is_watched_by_an_agent_on_a_routine() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let chef = call("agents:create", json!({ "name": "Chef", "instructions": "Cooks." }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();

        // The main agent (Filo) by default, each day at 8:00; or the agent and interval picked.
        call("feed:addTopicMine", json!({ "label": "Lisbon" })).unwrap();
        call("feed:addTopicMine", json!({ "label": "Pasta", "agentId": chef, "intervalMinutes": 120.0 })).unwrap();
        let topics = call("feed:topicsMine", json!({})).unwrap();
        let named = |label: &str| {
            call("feed:topicsMine", json!({}))
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .find(|topic| topic["label"] == label)
                .cloned()
                .unwrap()
        };
        let pasta = &named("Pasta");
        assert_eq!(
            (pasta["agentId"].as_str(), pasta["agentName"].as_str(), pasta["intervalMinutes"].as_i64()),
            (Some(chef.as_str()), Some("Chef"), Some(120))
        );
        let lisbon = &named("Lisbon");
        assert_eq!(topics.as_array().unwrap().len(), 2);
        assert_eq!(
            (lisbon["agentId"].as_str(), lisbon["pickMinute"].as_i64(), lisbon["refreshing"].as_bool()),
            (Some(super::super::DEFAULT_AGENT_ID), Some(480), Some(false))
        );
        let routines = call("routines:myRoutines", json!({})).unwrap();
        let mut titles: Vec<&str> =
            routines.as_array().unwrap().iter().map(|routine| routine["title"].as_str().unwrap()).collect();
        titles.sort();
        assert_eq!(titles, ["Feed: Lisbon", "Feed: Pasta"]);
        let pasta_routine =
            routines.as_array().unwrap().iter().find(|routine| routine["title"] == "Feed: Pasta").unwrap();
        assert!(pasta_routine["instructions"].as_str().unwrap().contains("`topic` exactly \u{201c}Pasta\u{201d}"));
        assert_eq!(pasta_routine["agentId"], json!(chef));
        assert!(call("feed:addTopicMine", json!({ "label": "Too often", "intervalMinutes": 5.0 })).is_err());
        assert!(call("feed:addTopicMine", json!({ "label": "Nobody", "agentId": "gone" })).is_err());
        assert_eq!(call("feed:topicsMine", json!({})).unwrap().as_array().unwrap().len(), 2);

        // Another agent, another time; renamed, its routine looks for the new name.
        let lisbon_key = lisbon["topicKey"].as_str().unwrap().to_string();
        call("feed:updateTopicMine", json!({ "topicKey": lisbon_key, "agentId": chef, "pickMinute": 600.0 })).unwrap();
        call("feed:renameTopicMine", json!({ "topicKey": lisbon_key, "label": "Lisbon jazz" })).unwrap();
        let lisbon = named("Lisbon jazz");
        assert_eq!((lisbon["agentName"].as_str(), lisbon["pickMinute"].as_i64()), (Some("Chef"), Some(600)));
        let routine = call("routines:myRoutine", json!({ "id": lisbon["routineId"] })).unwrap();
        assert_eq!(routine["routine"]["title"], "Feed: Lisbon jazz");
        assert!(routine["routine"]["instructions"].as_str().unwrap().contains("Lisbon jazz"));
        let settings = call("feed:settingsMine", json!({})).unwrap();
        assert_eq!((settings["intervalMinutes"].as_i64(), settings["pickMinute"].as_i64()), (Some(120), Some(600)));

        // Its routine deleted on the Scheduled page, the topic gets a new one when an agent is picked again.
        call("routines:deleteMine", json!({ "id": lisbon["routineId"] })).unwrap();
        assert_eq!(named("Lisbon jazz").get("agentId"), None);
        call("feed:updateTopicMine", json!({ "topicKey": lisbon_key, "agentId": chef })).unwrap();
        assert_eq!(named("Lisbon jazz")["agentName"], "Chef");

        // Removing a topic removes its routine.
        call("feed:removeTopicMine", json!({ "topicKey": lisbon_key })).unwrap();
        assert_eq!(call("routines:myRoutines", json!({})).unwrap().as_array().unwrap().len(), 1);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn refresh_runs_the_topics_routines() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let key =
            call("feed:addTopicMine", json!({ "label": "Lisbon" })).unwrap()["topicKey"].as_str().unwrap().to_string();
        // A run is in progress (another process's, say): Refresh says so rather than starting another.
        let routine = call("feed:topicsMine", json!({})).unwrap()[0]["routineId"].as_str().unwrap().to_string();
        let (_, run) = schedule::start_run_now(local.db().unwrap(), &routine, None, now_ms()).unwrap().unwrap();
        assert_eq!(call("feed:topicsMine", json!({})).unwrap()[0]["refreshing"], true);
        assert_eq!(call("feed:settingsMine", json!({})).unwrap()["running"], true);
        assert_eq!(call("feed:refreshMine", json!({})).unwrap(), "running");
        assert_eq!(call("feed:refreshTopicMine", json!({ "topicKey": key })).unwrap(), "running");
        // It ended a moment ago: what it found, and Refresh with ifStale leaves it be.
        publish(&local, "Fado", "https://example.com/fado", "article", None);
        let db = local.db().unwrap();
        db.with(|conn| conn.execute("UPDATE feed_posts SET topic_key = ?1", [&key])).unwrap();
        schedule::run_finished(db, &run.id, Some("Published 1."), None, now_ms() + 1).unwrap();
        let topic = call("feed:topicsMine", json!({})).unwrap()[0].clone();
        assert_eq!((topic["refreshing"].as_bool(), topic["lastRefreshPostCount"].as_i64()), (Some(false), Some(1)));
        let settings = call("feed:settingsMine", json!({})).unwrap();
        assert_eq!(settings["failedRuns"].as_i64(), Some(0));
        assert_eq!(call("feed:refreshMine", json!({ "ifStale": true })).unwrap(), "fresh");
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn topics_from_the_old_check_carry_over_to_the_main_agent() {
        let local = temp();
        let db = local.db().unwrap();
        let chef = local
            .call("agents:create", &json!({ "name": "Chef", "instructions": "Cooks." }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        local.call("users:setMainAgent", &json!({ "agentId": chef })).unwrap();
        feed::add_topic(db, "Lisbon", 1).unwrap();
        feed::add_topic(db, "Jazz", 2).unwrap();
        db.with(|conn| conn.execute("INSERT OR IGNORE INTO feed_settings (id) VALUES (1)", [])).unwrap();
        db.with(|conn| conn.execute("UPDATE feed_settings SET interval_minutes = 15", [])).unwrap();
        // Not while the Feed is off: its topics get their routines when it's turned on.
        local.set_page(filo_local::pages::Page::Feed, false).unwrap();
        local.feed_carry_over();
        assert!(local.call("routines:myRoutines", &json!({})).unwrap().as_array().unwrap().is_empty());
        local.set_page(filo_local::pages::Page::Feed, true).unwrap();
        let topics = local.call("feed:topicsMine", &json!({})).unwrap();
        for topic in topics.as_array().unwrap() {
            // 15 minutes was the Feed's; a routine runs every 30 at most.
            assert_eq!((topic["agentId"].as_str(), topic["intervalMinutes"].as_i64()), (Some(chef.as_str()), Some(30)));
        }
        // Once: nothing more next time.
        local.feed_carry_over();
        assert_eq!(local.call("routines:myRoutines", &json!({})).unwrap().as_array().unwrap().len(), 2);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn ask_goes_to_the_agent_feed_settings_pick_else_the_main_agent() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let post = publish(&local, "Fado night", "https://example.com/fado", "article", None);
        assert_eq!(call("feed:settingsMine", json!({})).unwrap()["askAgentId"], super::super::DEFAULT_AGENT_ID);
        let chef = call("agents:create", json!({ "name": "Chef", "instructions": "Cooks." }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let settings = call("feed:updateSettingsMine", json!({ "askAgentId": chef })).unwrap();
        assert_eq!(
            (settings["askAgentName"].as_str(), settings["askAgentPicked"].as_bool()),
            (Some("Chef"), Some(true))
        );
        assert!(call("feed:updateSettingsMine", json!({ "askAgentId": "gone" })).is_err());

        // The post page's Ask: a message in Chef's own conversation, the post kept for the engine (its turn isn't
        // started here: that would run Claude Code).
        let (session, agent, _) = local.ask_message(&post, None, None).unwrap();
        assert_eq!(agent, chef);
        assert_eq!(local.call("agents:list", &json!({})).unwrap()[1]["sessionId"], json!(session));
        let messages = local.agent_chats.messages(&session).unwrap();
        assert_eq!(messages[0].text, "Tell me more about \u{201c}Fado night\u{201d}.");
        let context =
            std::fs::read_to_string(local.root.join("agents").join("chats").join(&session).join("context.json"))
                .unwrap();
        assert!(context.contains("Title: Fado night") && context.contains("Shared by: Scout"));
        // Asked of another agent, it goes there; with the pick cleared, back to the main agent.
        let (filo, _, _) = local.ask_message(&post, Some("Worth it?"), Some(super::super::DEFAULT_AGENT_ID)).unwrap();
        assert_ne!(filo, session);
        assert_eq!(local.agent_chats.messages(&filo).unwrap()[0].text, "Worth it?");
        assert!(call("feed:askAboutPost", json!({ "postId": post, "agentId": "gone" })).is_err());
        call("feed:updateSettingsMine", json!({ "askAgentId": null })).unwrap();
        assert_eq!(call("feed:settingsMine", json!({})).unwrap()["askAgentPicked"], false);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_pick_in_a_glance_goes_back_to_the_conversation_that_posted_it() {
        let local = temp();
        let call = |path: &str, args: Value| local.call(path, &args);
        let chef = call("agents:create", json!({ "name": "Chef", "instructions": "Cooks." }))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let session = call("agents:openChat", json!({ "id": chef })).unwrap().as_str().unwrap().to_string();
        let db_path = local.root.join(filo_local::db::FILE);
        let origin = filo_local::feed_mcp::origin_in_chat(&db_path, &session);
        let glance = filo_local::glances::checked(&filo_local::glances::New {
            title: "Dinner tonight?".into(),
            html: "<p>Pick</p>".into(),
            text: "Pasta or soup.".into(),
            ..Default::default()
        })
        .unwrap();
        let new = feed::NewPost { note: "Your call.".into(), ..Default::default() };
        let post = feed::publish_glance(local.db().unwrap(), &new, &glance, &origin, now_ms()).unwrap();
        let listed = &call("feed:mine", json!({})).unwrap()["posts"][0];
        assert_eq!(
            (listed["kind"].as_str(), listed["glance"]["title"].as_str(), listed["authorAgentId"].as_str()),
            (Some("glance"), Some("Dinner tonight?"), Some(chef.as_str()))
        );
        assert!(listed["glance"]["glanceId"].as_str().is_some());
        assert_eq!(
            call("glances:get", json!({ "glanceId": listed["glance"]["glanceId"] })).unwrap()["text"],
            "Pasta or soup."
        );

        // The message is from the user, in Chef's conversation, with the post for the engine (its turn isn't started here:
        // that would run Claude Code).
        let (to, author, turn_id, _) = local.glance_choice_message(&post.id, "  Pasta  ").unwrap();
        assert_eq!((to.as_str(), author.as_str()), (session.as_str(), "Chef"));
        let messages = local.agent_chats.messages(&session).unwrap();
        assert_eq!(
            (messages[0].role, messages[0].text.as_str()),
            (
                filo_local::Role::User,
                "I picked \u{201c}Pasta\u{201d} on your glance \u{201c}Dinner tonight?\u{201d} in the Feed."
            )
        );
        assert_eq!(messages[0].turn.as_deref(), Some(turn_id.as_str()));
        let context =
            std::fs::read_to_string(local.root.join("agents").join("chats").join(&session).join("context.json"))
                .unwrap();
        assert!(context.contains("Pasta or soup.") && context.contains("update_feed_glance"), "{context}");
        assert_eq!(call("feed:mine", json!({})).unwrap()["posts"][0]["glanceChoice"], "Pasta");

        // Saved, the note has what the glance shows; nothing to pick in a link post.
        let saved = call("feed:setSavedMine", json!({ "postId": post.id, "saved": true })).unwrap();
        let note = notes::get(local.db().unwrap(), saved["noteId"].as_str().unwrap()).unwrap().unwrap();
        assert!(note.body.contains("Pasta or soup."));
        let link = publish(&local, "A link", "https://example.com/l", "article", None);
        assert!(local.glance_choice_message(&link, "x").is_err());
        assert!(local.glance_choice_message(&post.id, "   ").is_err());
        assert!(call("feed:chooseInGlance", json!({ "postId": "gone", "choice": "x" })).is_err());
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn posts_say_which_agent_shared_them() {
        let local = temp();
        let chef = local
            .call("agents:create", &json!({ "name": "Chef", "instructions": "Cooks.", "picture": "mint" }))
            .unwrap();
        let session = local.call("agents:openChat", &json!({ "id": chef })).unwrap().as_str().unwrap().to_string();
        let origin = filo_local::feed_mcp::origin_in_chat(&local.root.join(filo_local::db::FILE), &session);
        let new =
            feed::NewPost { title: "Carbonara".into(), url: "https://example.com/c".into(), ..Default::default() };
        let id = feed::publish(local.db().unwrap(), &new, &origin, now_ms()).unwrap().id;
        let post = local.call("feed:mine", &json!({})).unwrap()["posts"][0].clone();
        assert_eq!(
            (post["author"].as_str(), post["authorAgentId"].as_str(), post["authorPicture"].as_str()),
            (Some("Chef"), chef.as_str(), Some("mint"))
        );
        assert_eq!(local.call("feed:postMine", &json!({ "postId": id })).unwrap()["authorAgentId"], chef);
        std::fs::remove_dir_all(&local.root).ok();
    }

    #[test]
    fn a_message_about_posts_keeps_them_for_the_engine_only() {
        let local = temp();
        local.no_turns.store(true, std::sync::atomic::Ordering::SeqCst);
        let chat = local.agent_chats.create_chat(filo_local::Engine::Claude).unwrap().id;
        let chat = chat.as_str();
        let args = json!({ "sessionId": chat, "requestId": "req-1", "message": "Is it good?", "contextNote": "Title: Fado night" });
        let turn = local.call("voiceSessions:sendMessage", &args).unwrap();
        assert_eq!(turn, json!({ "turnId": "req-1", "sessionId": chat }));
        // Sent again (a retry): the same turn, nothing added.
        local.call("voiceSessions:sendMessage", &args).unwrap();
        let messages = local.agent_chats.messages(chat).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "Is it good?");
        let context =
            std::fs::read_to_string(local.agent_chats.root().join("chats").join(chat).join("context.json")).unwrap();
        assert!(context.contains("Title: Fado night"));
        // A Reply about posts keeps its quote.
        let reply = json!({ "sessionId": chat, "requestId": "req-0", "message": "And this?", "contextNote": "Title: Tram 28", "replyToMessageId": messages[0].id });
        local.call("voiceSessions:sendMessage", &reply).unwrap();
        assert_eq!(
            local.agent_chats.messages(chat).unwrap().last().unwrap().reply_to.as_deref(),
            Some(messages[0].id.as_str())
        );
        // A note shared into it goes the same way (the composer sends one with this), with the posts.
        let shared = json!({ "sessionId": chat, "requestId": "req-2", "message": "Add these?", "contextNote": "Title: Tram 28", "sharedNotes": [{ "id": "n-1", "title": "Lisbon", "body": "- Alfama" }] });
        local.call("voiceSessions:sendMessage", &shared).unwrap();
        assert_eq!(local.agent_chats.messages(chat).unwrap().last().unwrap().text, "Add these?");
        let context =
            std::fs::read_to_string(local.agent_chats.root().join("chats").join(chat).join("context.json")).unwrap();
        let context: Value = serde_json::from_str(&context).unwrap();
        let context = context["req-2"].as_str().unwrap();
        assert!(
            context.starts_with("Title: Tram 28\n\nA note the user shared from their Notes (id n-1")
                && context.ends_with("Title: Lisbon\n\n- Alfama"),
            "{context}"
        );
        std::fs::remove_dir_all(&local.root).ok();
    }
}

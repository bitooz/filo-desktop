// The optional pages, Feed and Notes (Settings › Pages, and the welcome's): each on or off, in Filo's database.
//
// Off, a page's tab, every way to it and its agent tools are gone (mcp.rs reads `Pages` once for its tools and the
// prompt's lines), but nothing of it is deleted: the notes and posts come back when it's on again. Feed off also
// pauses the topics' routines (feed.rs `routine_id`), the ones that were running, and turning it on resumes just those.
//
// Nothing saved means on: a Filo from before the switches had both. A new install's welcome turns both off
// (`start_off`), unless the page has something in it already.

use chrono::TimeZone;
use rusqlite::{OptionalExtension as _, params};
use serde_json::json;

use crate::db::Db;

/// Beside the migrations, made when first written (`set`), so a Filo that doesn't know it still opens the file.
const TABLE: &str = "CREATE TABLE IF NOT EXISTS page_settings (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL)";
/// The topic routines Feed off paused, as a JSON list of ids.
const PAUSED_ROUTINES: &str = "feed_paused_routines";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Feed,
    Notes,
}

impl Page {
    pub const ALL: [Page; 2] = [Page::Feed, Page::Notes];

    fn key(self) -> &'static str {
        match self {
            Page::Feed => "feed",
            Page::Notes => "notes",
        }
    }

    pub fn parse(value: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|page| page.key() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Feed => "Feed",
            Page::Notes => "Notes",
        }
    }
}

/// Which pages are on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pages {
    pub feed: bool,
    pub notes: bool,
}

impl Default for Pages {
    fn default() -> Self {
        Pages { feed: true, notes: true }
    }
}

impl Pages {
    pub fn on(self, page: Page) -> bool {
        match page {
            Page::Feed => self.feed,
            Page::Notes => self.notes,
        }
    }

    fn with(mut self, page: Page, on: bool) -> Pages {
        match page {
            Page::Feed => self.feed = on,
            Page::Notes => self.notes = on,
        }
        self
    }
}

fn value(db: &Db, key: &str) -> Option<String> {
    // No table yet: nothing saved.
    db.with(|conn| conn.query_row("SELECT value FROM page_settings WHERE key = ?1", [key], |row| row.get(0)).optional())
        .ok()
        .flatten()
}

fn set_value(db: &Db, key: &str, value: &str) -> Result<(), String> {
    db.write(|tx| {
        tx.execute_batch(TABLE)?;
        tx.execute(
            "INSERT INTO page_settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        )
    })
    .map(|_| ())
}

/// The pages as saved (on when nothing is).
pub fn get(db: &Db) -> Pages {
    Page::ALL
        .into_iter()
        .fold(Pages::default(), |pages, page| pages.with(page, value(db, page.key()).is_none_or(|value| value != "0")))
}

/// Turns `page` on or off. Feed off pauses its topics' routines that are running; on again resumes those (a one-off
/// whose time went by while it was off stays paused).
pub fn set<Tz: TimeZone>(db: &Db, tz: &Tz, page: Page, on: bool, now: i64) -> Result<(), String> {
    if page == Page::Feed && get(db).feed != on {
        if on {
            let paused: Vec<String> =
                value(db, PAUSED_ROUTINES).and_then(|ids| serde_json::from_str(&ids).ok()).unwrap_or_default();
            for id in &paused {
                let still_paused =
                    crate::schedule::routine(db, id, None)?.is_some_and(|routine| routine.status == "paused");
                if still_paused
                    && let Err(error) =
                        crate::schedule::update_routine(db, tz, id, &json!({ "status": "active" }), None, now)
                {
                    eprintln!("filo pages: a Feed topic's routine stays paused: {error}");
                }
            }
            set_value(db, PAUSED_ROUTINES, "[]")?;
        } else {
            let mut paused = Vec::new();
            for routine_id in crate::feed::topics(db)?.into_iter().filter_map(|topic| topic.routine_id) {
                let running = crate::schedule::routine(db, &routine_id, None)?.is_some_and(|routine| routine.active());
                if running
                    && crate::schedule::update_routine(db, tz, &routine_id, &json!({ "status": "paused" }), None, now)?
                {
                    paused.push(routine_id);
                }
            }
            set_value(db, PAUSED_ROUTINES, &serde_json::to_string(&paused).unwrap_or_else(|_| "[]".into()))?;
        }
    }
    set_value(db, page.key(), if on { "1" } else { "0" })
}

/// A new install (the first welcome): each page nobody has turned on or off yet starts off, unless it has something in
/// it (notes, or the Feed's topics or posts), so a Filo used before keeps what it had.
pub fn start_off<Tz: TimeZone>(db: &Db, tz: &Tz, now: i64) -> Result<(), String> {
    for page in Page::ALL {
        if value(db, page.key()).is_some() {
            continue;
        }
        let used = db.with(|conn| match page {
            Page::Notes => conn.query_row("SELECT EXISTS (SELECT 1 FROM notes)", [], |row| row.get::<_, bool>(0)),
            Page::Feed => conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM feed_topics) OR EXISTS (SELECT 1 FROM feed_posts)",
                [],
                |row| row.get::<_, bool>(0),
            ),
        })?;
        set(db, tz, page, used, now)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::NewRoutine;

    fn routine(db: &Db, title: &str) -> String {
        let new = NewRoutine {
            title: title.into(),
            instructions: "Look for news.".into(),
            days: json!([1, 2, 3, 4, 5]),
            minute_of_day: json!(480),
            ..Default::default()
        };
        crate::schedule::create_routine(db, &chrono::Utc, new, 1_000).unwrap().id
    }

    #[test]
    fn pages_are_on_until_turned_off_and_a_new_install_starts_them_off_unless_used() {
        let db = Db::in_memory().unwrap();
        assert_eq!(get(&db), Pages { feed: true, notes: true });
        set(&db, &chrono::Utc, Page::Notes, false, 1).unwrap();
        assert_eq!(get(&db), Pages { feed: true, notes: false });
        // Already chosen: the welcome leaves it.
        set(&db, &chrono::Utc, Page::Notes, true, 2).unwrap();
        start_off(&db, &chrono::Utc, 3).unwrap();
        assert_eq!(get(&db), Pages { feed: false, notes: true });

        // A Mac with notes on it keeps Notes on.
        let used = Db::in_memory().unwrap();
        crate::notes::create(
            &used,
            &crate::notes::Fields { title: "Trip".into(), body: "Passport".into(), ..Default::default() },
        )
        .unwrap();
        start_off(&used, &chrono::Utc, 2).unwrap();
        assert_eq!(get(&used), Pages { feed: false, notes: true });
        assert_eq!(Page::parse("feed"), Some(Page::Feed));
        assert_eq!(Page::parse("chat"), None);
    }

    #[test]
    fn feed_off_pauses_the_topics_running_routines_and_on_resumes_just_those() {
        let db = Db::in_memory().unwrap();
        let (running, stopped, own) =
            (routine(&db, "Feed: Rust"), routine(&db, "Feed: Go"), routine(&db, "Morning brief"));
        crate::schedule::update_routine(&db, &chrono::Utc, &stopped, &json!({ "status": "paused" }), None, 2).unwrap();
        for (label, id) in [("Rust", &running), ("Go", &stopped)] {
            let key = crate::feed::add_topic(&db, label, 1).unwrap();
            crate::feed::set_topic_routine(&db, &key, Some(id)).unwrap();
        }
        let status = |id: &str| crate::schedule::routine(&db, id, None).unwrap().unwrap().status;

        set(&db, &chrono::Utc, Page::Feed, false, 3).unwrap();
        assert_eq!(
            (status(&running).as_str(), status(&stopped).as_str(), status(&own).as_str()),
            ("paused", "paused", "active")
        );
        // Off again changes nothing.
        set(&db, &chrono::Utc, Page::Feed, false, 4).unwrap();
        set(&db, &chrono::Utc, Page::Feed, true, 5).unwrap();
        assert_eq!(
            (status(&running).as_str(), status(&stopped).as_str(), status(&own).as_str()),
            ("active", "paused", "active")
        );
        assert!(crate::schedule::routine(&db, &running, None).unwrap().unwrap().next_run_at.is_some());
        // The topics and their routines were never removed.
        assert_eq!(crate::feed::topics(&db).unwrap().len(), 2);
    }
}

// Routine rows and labels shared by Scheduled and the routine page.

use serde::Deserialize;

use crate::format;

pub const WEEKDAY_LETTERS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
const WEEKDAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
pub const WEEKDAY_FULL_NAMES: [&str; 7] =
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// Once or every N minutes; absent means weekly on `days` at `minute_of_day`.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Schedule {
    Once { at: f64 },
    Interval { every_minutes: f64, from_minute: Option<f64>, to_minute: Option<f64> },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    #[serde(rename = "_id")]
    pub id: String,
    pub title: String,
    pub instructions: String,
    pub days: Vec<f64>,
    pub minute_of_day: f64,
    pub status: String,
    pub next_run_at: Option<f64>,
    pub last_skip_reason: Option<String>,
    pub schedule: Option<Schedule>,
    /// The agent that owns it (its runs happen in the agent's conversation); none for Filo's.
    #[serde(default)]
    pub agent_id: Option<String>,
}

impl Routine {
    pub fn active(&self) -> bool {
        self.status == "active"
    }

    pub fn has_day(&self, day: u32) -> bool {
        self.days.iter().any(|value| *value as u32 == day)
    }
}

/// Wall-clock time of the routine ("8:00 AM").
pub fn format_routine_time(minute_of_day: f64) -> String {
    format::minute_of_day(minute_of_day)
}

pub fn format_routine_days(days: &[f64]) -> String {
    let mut set: Vec<usize> = days.iter().map(|day| (*day as usize).min(6)).collect();
    set.sort_unstable();
    set.dedup();
    let has = |day: usize| set.contains(&day);
    if set.len() == 7 {
        return "Every day".into();
    }
    if set.len() == 5 && (1..=5).all(has) {
        return "Weekdays".into();
    }
    if set.len() == 2 && has(0) && has(6) {
        return "Weekends".into();
    }
    if set.len() == 1 {
        return format!("Every {}", WEEKDAY_FULL_NAMES[set[0]]);
    }
    // Monday-first reads naturally; Sunday goes last.
    set.sort_by_key(|day| (day + 6) % 7);
    set.iter().map(|day| WEEKDAY_NAMES[*day]).collect::<Vec<_>>().join(", ")
}

/// "every 30 min", "every hour", "every 2 hours", "every day": a routine's and a Feed topic's interval, in a sentence.
pub(crate) fn interval_words(minutes: f64) -> String {
    let minutes = minutes.round() as i64;
    if minutes > 0 && minutes % (24 * 60) == 0 {
        return if minutes == 24 * 60 { "every day".into() } else { format!("every {} days", minutes / (24 * 60)) };
    }
    if minutes > 0 && minutes % 60 == 0 {
        return if minutes == 60 { "every hour".into() } else { format!("every {} hours", minutes / 60) };
    }
    format!("every {minutes} min")
}

/// `interval_words` on its own line: "Every hour".
pub(crate) fn format_interval(minutes: f64) -> String {
    let words = interval_words(minutes);
    let mut chars = words.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `formatRoutineSchedule`. A one-off time is shown in the computer's time
/// zone (the app uses the routine's zone, which is normally the same).
pub fn format_routine_schedule(routine: &Routine) -> String {
    match &routine.schedule {
        Some(Schedule::Once { at }) => format!("Once · {}", format::weekday_date_time(*at)),
        Some(Schedule::Interval { every_minutes, from_minute, to_minute }) => {
            let window = match (from_minute, to_minute) {
                (Some(from), Some(to)) => format!(" · {}–{}", format_routine_time(*from), format_routine_time(*to)),
                _ => String::new(),
            };
            format!("{}{window}", format_interval(*every_minutes))
        }
        None => format!("{} · {}", format_routine_days(&routine.days), format_routine_time(routine.minute_of_day)),
    }
}

pub fn routine_run_label(state: &str) -> String {
    match state {
        "completed" => "Ready",
        "queued" | "active" => "Working…",
        "failed" => "Failed",
        other => other,
    }
    .to_string()
}

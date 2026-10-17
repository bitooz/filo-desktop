// Date and size labels the app shows (reminders, feed ages, chat day pills, note dates), in the computer's local time
// zone.

use chrono::{DateTime, Datelike as _, Local, TimeZone as _};

pub fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

/// How long a chat's work has run, for "Working 3m": seconds in the first
/// minute, then minutes, then hours (`hours_minutes`).
pub fn working_for(started_ms: Option<f64>, now_ms: f64) -> String {
    let Some(started) = started_ms else { return String::new() };
    let seconds = ((now_ms - started) / 1_000.).floor().max(0.) as i64;
    let minutes = seconds / 60;
    match minutes {
        0 => format!("{seconds}s"),
        1..60 => format!("{minutes}m"),
        _ => hours_minutes(minutes),
    }
}

/// "1h" on the hour, "1h 5m" otherwise: how every time over an hour reads, working or worked, so a list's rows match
/// ("Working · 1h 5m" over "Done · 1h 5m").
pub fn hours_minutes(minutes: i64) -> String {
    match minutes % 60 {
        0 => format!("{}h", minutes / 60),
        rest => format!("{}h {rest}m", minutes / 60),
    }
}

pub fn local(ms: f64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(Local::now)
}

/// "3:07 PM"
pub fn clock(ms: f64) -> String {
    local(ms).format("%-I:%M %p").to_string()
}

pub fn same_day(a: DateTime<Local>, b: DateTime<Local>) -> bool {
    a.date_naive() == b.date_naive()
}

/// Local calendar day, for grouping.
pub fn day_key(ms: f64) -> chrono::NaiveDate {
    local(ms).date_naive()
}

/// "Today", "Yesterday", "October 2", or "October 2, 2025" for other years.
pub fn day_label(ms: f64) -> String {
    let date = local(ms);
    let today = Local::now();
    if same_day(date, today) {
        return "Today".into();
    }
    if same_day(date + chrono::Duration::days(1), today) {
        return "Yesterday".into();
    }
    if date.year() == today.year() { date.format("%B %-d").to_string() } else { date.format("%B %-d, %Y").to_string() }
}

/// "Today · 3:07 PM", "October 2 · 9:14 AM": a chat's time marker on a new day.
pub fn day_time(ms: f64) -> String {
    format!("{} · {}", day_label(ms), clock(ms))
}

/// For a list row: "3:07 PM" today, "Mon" this week, else "Oct 2".
pub fn list_time(ms: f64, now: f64) -> String {
    let (then, today) = (local(ms), local(now));
    if same_day(then, today) {
        return clock(ms);
    }
    if then.date_naive() > today.date_naive() - chrono::Duration::days(7) {
        return then.format("%a").to_string();
    }
    short_date(ms)
}

/// "Oct 2" / "Oct 2, 3:45 PM"
pub fn short_date(ms: f64) -> String {
    local(ms).format("%b %-d").to_string()
}

pub fn short_date_time(ms: f64) -> String {
    local(ms).format("%b %-d, %-I:%M %p").to_string()
}

/// "Thu, Oct 2, 3:45 PM"
pub fn weekday_date_time(ms: f64) -> String {
    local(ms).format("%a, %b %-d, %-I:%M %p").to_string()
}

/// When a reminder is due, as its row says it.
pub fn reminder_due(due_at: f64, now: f64) -> String {
    let clock = clock(due_at);
    if due_at < now {
        let minutes = ((now - due_at) / 60_000.).round().max(1.) as i64;
        if minutes < 60 {
            return format!("{minutes} min overdue · {clock}");
        }
        let hours = ((minutes as f64) / 60.).round().max(1.) as i64;
        if hours < 24 {
            return format!("{hours} hr overdue · {clock}");
        }
        return format!("Overdue · {}", short_date(due_at));
    }
    let due = local(due_at);
    let today = local(now);
    if same_day(due, today) {
        return format!("Today · {clock}");
    }
    if same_day(due, today + chrono::Duration::days(1)) {
        return format!("Tomorrow · {clock}");
    }
    format!("{} · {clock}", due.format("%a, %b %-d"))
}

/// How long ago `ms` was: under a minute, minutes, hours the same day, the day before, or the date.
enum Ago {
    JustNow,
    Minutes(i64),
    Hours(i64),
    Yesterday,
    On(String),
}

fn ago_of(ms: f64, now: f64) -> Ago {
    let minutes = ((now - ms) / 60_000.).floor().max(0.) as i64;
    let (then, today) = (local(ms), local(now));
    match minutes {
        0 => Ago::JustNow,
        1..60 => Ago::Minutes(minutes),
        _ if same_day(then, today) => Ago::Hours(minutes / 60),
        _ if same_day(then + chrono::Duration::days(1), today) => Ago::Yesterday,
        _ => Ago::On(short_date(ms)),
    }
}

/// Feed post age: "Just now", "12 min", "3 hr", "Yesterday", "Sep 28".
pub fn age(ms: f64, now: f64) -> String {
    match ago_of(ms, now) {
        Ago::JustNow => "Just now".into(),
        Ago::Minutes(minutes) => format!("{minutes} min"),
        Ago::Hours(hours) => format!("{hours} hr"),
        Ago::Yesterday => "Yesterday".into(),
        Ago::On(date) => date,
    }
}

/// The same in a sentence ("Updated 5 min ago", "Refreshed yesterday"): "just now", "5 min ago", "3 hr ago",
/// "yesterday", "on Sep 28".
pub fn ago(ms: f64, now: f64) -> String {
    match ago_of(ms, now) {
        Ago::JustNow => "just now".into(),
        Ago::Minutes(minutes) => format!("{minutes} min ago"),
        Ago::Hours(hours) => format!("{hours} hr ago"),
        Ago::Yesterday => "yesterday".into(),
        Ago::On(date) => format!("on {date}"),
    }
}

/// "1:05" (minutes:seconds), a recording's or a video's length.
pub fn duration_clock(ms: f64) -> String {
    minutes_seconds((ms / 1000.).round().max(0.) as i64)
}

/// "1:05" for 65 seconds: a length, or a recording's time so far (whole seconds, chat/voice.rs).
pub fn minutes_seconds(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// How long a coding task worked (`coding_handoff::worked_ms`), for its card and rows ("Done · 18m"): "45s", "12m",
/// "1h 5m".
pub fn worked_short(ms: f64) -> String {
    let seconds = (ms / 1000.).round().max(1.) as i64;
    match seconds {
        ..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => hours_minutes(seconds / 60),
    }
}

/// How long a round worked, for its result page's header: "45s", "12m 40s", "1h 5m".
pub fn worked_long(ms: f64) -> String {
    let seconds = (ms / 1000.).round().max(1.) as i64;
    match seconds {
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => worked_short(ms),
    }
}

/// A file's size: B, KB rounded, MB with one decimal ("12.4 MB"), the same wherever a file shows (chat, viewer, a
/// round's proof) and as the model is told it (crates/filo-local attachments.rs).
pub fn file_size(bytes: f64) -> String {
    filo_local::attachments::size_label(bytes.max(0.) as u64)
}

/// "h:mm AM" for a minute of the day (routines, feed pick time).
pub fn minute_of_day(minute: f64) -> String {
    let minute = minute.max(0.) as u32;
    let time = chrono::NaiveTime::from_hms_opt((minute / 60) % 24, minute % 60, 0).unwrap_or_default();
    time.format("%-I:%M %p").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_work_took() {
        let short: Vec<String> =
            [400., 45_000., 12. * 60_000. + 40_000., 65. * 60_000. + 30_000.].into_iter().map(worked_short).collect();
        assert_eq!(short, ["1s", "45s", "12m", "1h 5m"]);
        let long: Vec<String> =
            [45_000., 12. * 60_000. + 40_000., 65. * 60_000. + 30_000.].into_iter().map(worked_long).collect();
        assert_eq!(long, ["45s", "12m 40s", "1h 5m"]);
    }

    #[test]
    fn hours_read_without_a_padded_zero() {
        assert_eq!(hours_minutes(60), "1h");
        assert_eq!(hours_minutes(65), "1h 5m");
        assert_eq!(hours_minutes(130), "2h 10m");
    }

    #[test]
    fn working_and_worked_times_read_alike() {
        let minute = 60_000.;
        assert_eq!(working_for(Some(0.), 65. * minute + 30_000.), worked_short(65. * minute));
        assert_eq!(working_for(Some(0.), 120. * minute), "2h");
        assert_eq!(file_size(12.4 * 1024. * 1024.), "12.4 MB");
    }
}

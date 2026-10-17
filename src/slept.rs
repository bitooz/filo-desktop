// Telling the user when the Mac slept anyway while tasks kept it awake: a closed lid (without "Keep running with the lid
// closed"), the Apple menu's Sleep, a low battery. Coding tasks, routines and replies pause while it sleeps and carry on
// when it wakes; once someone's back at the Mac Filo says so: "Mac slept 5 min (lid closed). 2 tasks paused, then
// resumed."
//
// The app looks every few seconds (companion.rs, with the menu bar's `coding:keepingAwake`). macOS keeps when it last
// went to sleep and woke (`kern.sleeptime`, `kern.waketime`) and why it slept (IOPMrootDomain's "Last Sleep Reason"): a
// wake since the last look, with work keeping the Mac awake at that look, is a sleep that paused it. The message waits
// for input after the wake, so the short dark wakes of a closed Mac (Power Nap, maintenance) don't say it to nobody;
// the duration adds the recorded sleep intervals, excluding time awake between them. A lid closed while lid mode was on
// but the Mac on battery says that mode works plugged in only.

/// The work keeping the Mac awake as the app last saw it, and a sleep that paused some, until it's told.
#[derive(Default)]
pub struct Watch {
    last_wake: Option<i64>,
    held: Vec<String>,
    /// Whether the Mac was on AC power at the last look (before a sleep, the power it slept on).
    on_ac: bool,
    pending: Option<Pending>,
}

#[derive(Debug, PartialEq)]
struct Pending {
    /// Time asleep across dark wakes, and the last interval recorded (seconds since 1970).
    slept_secs: i64,
    last_sleep: i64,
    last_wake: i64,
    reason: Option<&'static str>,
    /// On battery when it went to sleep.
    on_battery: bool,
    /// The work it paused ("task:<id>", "turn:<id>").
    paused: Vec<String>,
}

impl Watch {
    /// One look: `work` is all running work, `keep_awake` whether it keeps the Mac awake, `lid_option` whether "Keep
    /// running with the lid closed" is on. The message, once, when a sleep paused work and the user is back.
    pub fn look(&mut self, work: &[String], keep_awake: bool, lid_option: bool) -> Option<String> {
        let (slept_at, woke_at) = sleep_and_wake()?;
        let back = crate::presence::idle_seconds() < (chrono::Utc::now().timestamp() - woke_at) as f64;
        self.look_at(
            work,
            slept_at,
            woke_at,
            last_sleep_reason,
            back,
            keep_awake,
            lid_option,
            crate::local_backend::on_ac_power(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn look_at(
        &mut self,
        work: &[String],
        slept_at: i64,
        woke_at: i64,
        reason: impl FnOnce() -> Option<&'static str>,
        back: bool,
        keep_awake: bool,
        lid_option: bool,
        on_ac: bool,
    ) -> Option<String> {
        // Before the first sleep since boot the kernel can report two zero timestamps. Learn the work and a wake
        // baseline anyway, so that first sleep gets a notice too.
        if slept_at == 0 && woke_at == 0 {
            self.last_wake.get_or_insert(0);
            self.held = if keep_awake { work.to_vec() } else { Vec::new() };
            self.on_ac = on_ac;
            return None;
        }
        // The two sysctls can briefly describe a new sleep and the previous wake. Wait for a complete pair, and do
        // not turn clock corrections or repeated wake samples into another interruption.
        if woke_at <= slept_at || self.last_wake.is_some_and(|last| woke_at < last) {
            return None;
        }
        if self.last_wake.is_some_and(|last| last < woke_at) {
            if let Some(pending) = &mut self.pending {
                // A new sleep adds its own duration. Another wake time for the same sleep adds nothing: that sleep
                // ended at the wake already counted, so the time since was spent awake.
                if pending.last_sleep != slept_at {
                    pending.slept_secs += woke_at - slept_at;
                }
                pending.last_sleep = slept_at;
                pending.last_wake = woke_at;
                // Work can change during a dark wake. Include any additional work the next sleep paused, once each.
                for id in &self.held {
                    if !pending.paused.contains(id) {
                        pending.paused.push(id.clone());
                    }
                }
            } else if !self.held.is_empty() {
                self.pending = Some(Pending {
                    slept_secs: woke_at - slept_at,
                    last_sleep: slept_at,
                    last_wake: woke_at,
                    reason: reason(),
                    on_battery: !self.on_ac,
                    paused: self.held.clone(),
                });
            }
        }
        self.last_wake = Some(woke_at);
        self.held = if keep_awake { work.to_vec() } else { Vec::new() };
        self.on_ac = on_ac;
        if !back {
            return None;
        }
        let pending = self.pending.take()?;
        let resumed = pending.paused.iter().filter(|paused| work.contains(paused)).count();
        let hint = match pending.reason {
            Some(LID_CLOSED) if !lid_option => Hint::TurnOnLidMode,
            Some(LID_CLOSED) if pending.on_battery => Hint::PlugIn,
            _ => Hint::None,
        };
        Some(message(pending.slept_secs, pending.reason, pending.paused.len(), resumed, hint))
    }
}

/// What a lid-closed sleep's message adds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hint {
    None,
    /// Lid mode is off.
    TurnOnLidMode,
    /// Lid mode is on, but the Mac was on battery.
    PlugIn,
}

const LID_CLOSED: &str = "lid closed";

/// "Mac slept 5 min (lid closed). 2 tasks paused, then resumed."
fn message(slept_secs: i64, reason: Option<&str>, paused: usize, resumed: usize, hint: Hint) -> String {
    let minutes = (slept_secs.max(0) + 30) / 60;
    let slept = match minutes {
        0 => "less than a minute".to_string(),
        1..=59 => format!("{minutes} min"),
        _ if minutes % 60 == 0 => format!("{} h", minutes / 60),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    };
    let why = reason.map(|reason| format!(" ({reason})")).unwrap_or_default();
    let tasks = if paused == 1 { "1 task".to_string() } else { format!("{paused} tasks") };
    let after = match resumed {
        _ if resumed == paused => "paused, then resumed.".to_string(),
        0 if paused == 1 => "paused; it has ended since.".to_string(),
        0 => "paused; they have ended since.".to_string(),
        _ => format!("paused; {resumed} resumed, the others have ended since."),
    };
    let hint = match hint {
        Hint::None => "",
        Hint::TurnOnLidMode => {
            " To keep them going with the lid closed on external power, turn on “Keep running with the lid closed” on the Coding page."
        }
        Hint::PlugIn => " The Mac was on battery: “Keep running with the lid closed” only works while it's plugged in.",
    };
    format!("Mac slept {slept}{why}. {tasks} {after}{hint}")
}

/// When the Mac last went to sleep and last woke, in seconds since 1970.
fn sleep_and_wake() -> Option<(i64, i64)> {
    Some((sysctl_time(c"kern.sleeptime")?, sysctl_time(c"kern.waketime")?))
}

fn sysctl_time(name: &std::ffi::CStr) -> Option<i64> {
    let mut time = libc::timeval { tv_sec: 0, tv_usec: 0 };
    let mut size = std::mem::size_of::<libc::timeval>();
    // SAFETY: a read of a timeval-sized value into one.
    let read = unsafe {
        libc::sysctlbyname(name.as_ptr(), (&mut time as *mut libc::timeval).cast(), &mut size, std::ptr::null_mut(), 0)
    };
    (read == 0 && time.tv_sec >= 0).then_some(time.tv_sec)
}

/// Why the Mac last slept, in words, from IOPMrootDomain's "Last Sleep Reason"; `None` for reasons that say nothing to
/// the user (or when it can't be read).
fn last_sleep_reason() -> Option<&'static str> {
    let output =
        std::process::Command::new("/usr/sbin/ioreg").args(["-n", "IOPMrootDomain", "-r", "-d1"]).output().ok()?;
    reason_in(&String::from_utf8_lossy(&output.stdout))
}

fn reason_in(ioreg: &str) -> Option<&'static str> {
    let line = ioreg.lines().find(|line| line.contains("\"Last Sleep Reason\""))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    match value {
        "Clamshell Sleep" => Some(LID_CLOSED),
        "Idle Sleep" => Some("idle"),
        "Software Sleep" => Some("put to sleep"),
        "Power Button Sleep" => Some("power button"),
        "Low Power Sleep" => Some("battery low"),
        "Thermal Emergency Sleep" => Some("too hot"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_how_long_why_and_what_carried_on() {
        // Tonight's: a clamshell sleep 01:03:46 → 01:08:45 with two coding tasks.
        assert_eq!(
            message(299, Some(LID_CLOSED), 2, 2, Hint::None),
            "Mac slept 5 min (lid closed). 2 tasks paused, then resumed."
        );
        assert_eq!(message(20, None, 1, 1, Hint::None), "Mac slept less than a minute. 1 task paused, then resumed.");
        assert_eq!(
            message(65 * 60, Some("put to sleep"), 3, 2, Hint::None),
            "Mac slept 1 h 5 min (put to sleep). 3 tasks paused; 2 resumed, the others have ended since."
        );
        assert_eq!(
            message(2 * 3600, Some("idle"), 1, 0, Hint::None),
            "Mac slept 2 h (idle). 1 task paused; it has ended since."
        );
        assert_eq!(message(600, None, 2, 0, Hint::None), "Mac slept 10 min. 2 tasks paused; they have ended since.");
        assert!(
            message(300, Some(LID_CLOSED), 1, 1, Hint::TurnOnLidMode)
                .ends_with("turn on “Keep running with the lid closed” on the Coding page.")
        );
    }

    #[test]
    fn reads_the_sleep_reason() {
        let ioreg = "+-o IOPMrootDomain\n    {\n      \"Last Sleep Reason\" = \"Clamshell Sleep\"\n      \"Wake Reason\" = \"rtc\"\n";
        assert_eq!(reason_in(ioreg), Some(LID_CLOSED));
        assert_eq!(reason_in("      \"Last Sleep Reason\" = \"Maintenance Sleep\""), None);
        assert_eq!(reason_in(""), None);
        // This Mac's own: there, and one of the known ones or not.
        let _ = last_sleep_reason();
        assert!(sleep_and_wake().is_some(), "kern.sleeptime and kern.waketime");
    }

    #[test]
    fn a_wake_with_work_held_is_told_once_the_user_is_back() {
        let work = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let mut watch = Watch::default();
        assert_eq!(
            watch.look_at(&work(&["task:a", "task:b"]), 100, 200, || None, true, true, false, true),
            None,
            "the first look only learns"
        );
        assert_eq!(
            watch.look_at(&work(&["task:a", "task:b"]), 100, 200, || None, true, true, false, true),
            None,
            "no wake since"
        );
        // Asleep 1000 → 1300, lid closed; a dark wake with nobody there first.
        assert_eq!(
            watch.look_at(&work(&["task:a", "task:b"]), 1000, 1060, || Some(LID_CLOSED), false, true, true, true),
            None
        );
        // Asleep again, then the user opens the lid: 60 + 200 seconds asleep, excluding 40 seconds awake.
        let told = watch.look_at(&work(&["task:a", "task:b"]), 1100, 1300, || Some("idle"), true, true, true, true);
        assert_eq!(told.as_deref(), Some("Mac slept 4 min (lid closed). 2 tasks paused, then resumed."));
        assert_eq!(watch.look_at(&work(&["task:a"]), 1100, 1300, || None, true, true, true, true), None, "once");
        // Nothing held when it slept: nothing to say.
        let mut idle = Watch::default();
        idle.look_at(&[], 0, 10, || None, true, true, false, true);
        assert_eq!(idle.look_at(&[], 20, 500, || Some(LID_CLOSED), true, true, false, true), None);
    }

    #[test]
    fn delayed_notices_count_newly_interrupted_work_without_treating_disabled_awake_as_ended() {
        let work = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let mut watch = Watch::default();
        watch.look_at(&work(&["task:a"]), 100, 200, || None, true, true, true, true);
        watch.look_at(&work(&["task:a", "turn:b"]), 1000, 1060, || Some(LID_CLOSED), false, true, true, true);
        let told = watch.look_at(&work(&["turn:b", "turn:c"]), 1100, 1300, || Some("idle"), true, false, true, true);
        assert_eq!(
            told.as_deref(),
            Some("Mac slept 4 min (lid closed). 2 tasks paused; 1 resumed, the others have ended since.")
        );
        assert_eq!(watch.look_at(&work(&["turn:b"]), 1400, 1500, || Some("idle"), true, false, true, true), None);
    }

    #[test]
    fn incomplete_sleep_pairs_and_backwards_wakes_do_not_create_notices() {
        let work = vec!["turn:a".to_string()];
        let mut watch = Watch::default();
        watch.look_at(&work, 100, 200, || None, true, true, true, true);
        assert_eq!(watch.look_at(&work, 1000, 200, || Some(LID_CLOSED), true, true, true, true), None);
        assert_eq!(
            watch.look_at(&work, 1000, 1300, || Some(LID_CLOSED), true, true, true, true).as_deref(),
            Some("Mac slept 5 min (lid closed). 1 task paused, then resumed.")
        );
        assert_eq!(watch.look_at(&work, 1000, 1200, || Some(LID_CLOSED), true, true, true, true), None);
    }

    #[test]
    fn dark_wake_duration_counts_each_interval_once_even_after_work_ends() {
        let work = vec!["turn:a".to_string()];
        let mut watch = Watch::default();
        watch.look_at(&work, 100, 200, || None, true, true, true, true);
        watch.look_at(&work, 1000, 1060, || Some(LID_CLOSED), false, true, true, true);
        // A later wake time for the same sleep (1000): the minute since 1060 was spent awake, not asleep.
        watch.look_at(&[], 1000, 1120, || None, false, true, true, true);
        watch.look_at(&[], 1000, 1120, || None, false, true, true, true);
        // Ten minutes awake with no input, then one minute asleep again: 60 + 60 seconds, not 13 minutes.
        let told = watch.look_at(&[], 1720, 1780, || None, true, true, true, true);
        assert_eq!(told.as_deref(), Some("Mac slept 2 min (lid closed). 1 task paused; it has ended since."));
    }

    #[test]
    fn a_lid_closed_on_battery_with_lid_mode_on_says_it_needs_power() {
        let work = vec!["task:a".to_string()];
        let mut watch = Watch::default();
        watch.look_at(&work, 100, 200, || None, true, true, true, false);
        let told = watch.look_at(&work, 1000, 1300, || Some(LID_CLOSED), true, true, true, true).unwrap();
        assert!(told.ends_with("“Keep running with the lid closed” only works while it's plugged in."), "{told}");
        // Plugged in when it slept: no hint (something else slept it).
        watch.look_at(&work, 2000, 2300, || Some(LID_CLOSED), true, true, true, true);
        let told = watch.look_at(&work, 3000, 3300, || Some(LID_CLOSED), true, true, true, true).unwrap();
        assert_eq!(told, "Mac slept 5 min (lid closed). 1 task paused, then resumed.");
        // Lid mode off: the hint to turn it on, whatever the power.
        let told = watch.look_at(&work, 4000, 4300, || Some(LID_CLOSED), true, true, false, false).unwrap();
        assert!(told.ends_with("on the Coding page."), "{told}");
    }

    #[test]
    fn the_first_sleep_since_boot_is_reported() {
        let work = vec!["turn:a".to_string()];
        let mut watch = Watch::default();
        assert_eq!(watch.look_at(&work, 0, 0, || None, true, true, true, true), None);
        assert_eq!(
            watch.look_at(&work, 100, 280, || Some("idle"), true, true, true, true).as_deref(),
            Some("Mac slept 3 min (idle). 1 task paused, then resumed.")
        );
    }
}

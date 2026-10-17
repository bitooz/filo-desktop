// Keeping tasks running with the lid closed: "Keep running with the lid closed (plugged in only)", under "Keep Mac awake
// while tasks run" on the Coding page (off until the user turns it on; crates/filo-local `coding::keep_awake_lid`).
//
// A power assertion (awake.rs) doesn't stop a clamshell sleep: closing the lid sleeps the Mac whatever holds one. What
// does is `pmset -a disablesleep 1`, which takes root. Modeled on Insomnia (github.com/krishhgg/Insomnia, MIT; see
// THIRD_PARTY_NOTICES.md): its sudoers rule, journal and launchd backstop.
//
// - Turning the option on asks for an administrator's password once (macOS's own prompt) to install /etc/sudoers.d/filo:
//   exactly `pmset -a disablesleep 1` and `pmset -a disablesleep 0` for this user, checked with `visudo -c` before it
//   goes in. Nothing else of Filo's ever runs as root, and `sudo -n` never asks for a password.
// - Sleep is the whole Mac's, so what Filo keeps about it is this user's, not a data folder's: `~/Library/Application
//   Support/Filo/awake/` (`shared_dir`, outside the folders macOS guards, so launchd's shell can open it), whichever
//   Filo runs. A Filo with a data folder of its own (`FILO_CONFIG_DIR`, checks and tests) never uses lid mode unless
//   `FILO_LAUNCH_AGENT=1` (launch_agent.rs `wanted`).
// - Filo sets `disablesleep 1` only while a process holds the keep-awake assertion with the option on for its data (it
//   keeps a holder's file locked there, naming its data folder: `Holder`), the Mac is on AC power, isn't at a critical
//   thermal state and sleep hasn't been off for `CAP` already; and back to 0 when the last such hold ends, the Mac goes
//   to battery, it gets critically hot, the cap is reached or the option goes off. Each process with holds looks every
//   couple of seconds (`reconcile`), one at a time under `lid.lock`, reading its option under the lock. Sleep the user
//   disabled themselves is left alone. When sleep comes back on with the lid closed, the Mac is put to sleep.
// - `lid.json`, the journal, says Filo disabled sleep. It's written before `pmset` runs and removed once sleep is back
//   on, so whatever happens in between, an undo is never forgotten. A failed `pmset` is retried less and less often
//   (`problem.json`, which the Coding page shows).
// - The backstop, a launchd agent, looks every minute and at login: when the journal says Filo disabled sleep and no
//   hold is active (Filo died, a worker was killed, nothing runs), or the Mac is on battery, or it's been `CAP`, it sets
//   sleep back to 0. It runs `filo --lid-backstop` only after `codesign --verify` passes against the requirement pinned
//   when it was installed (the app pins its own build again when it opens); for a binary that doesn't pass, a few lines
//   of shell do the same undo without any of Filo's code. Each run touches `backstop.ran`, which turning the option on
//   waits for and the Coding page warns about when it's old.
// - `disablesleep` is kept in the system's power settings (/Library/Preferences/com.apple.PowerManagement.plist), so it
//   survives a restart: the backstop's first run at login and the app's look when it opens set it back on. Before
//   anyone logs in (the login window) nothing of Filo's runs, so a Mac restarted while sleep was off stays awake there.
// - Turning the option off (or "Remove access") sets sleep back on, removes the backstop and, with the password prompt
//   again, the rule; never while the journal says sleep is off. If sudo stopped working meanwhile, that one prompt sets
//   sleep back on too. `filo --uninstall-lid` does the same from a terminal, for deleting the app.

use std::ffi::c_void;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Local, now_ms};

/// What's kept about sleep: holders' lock files, the journal and its lock, the last failure, the backstop's traces.
const JOURNAL: &str = "lid.json";
const LOCK: &str = "lid.lock";
const PROBLEM: &str = "problem.json";
/// There while lid mode has had sleep off for `CAP` and its holds haven't all ended since.
const CAPPED: &str = "capped";
/// Touched by each backstop run.
const RAN: &str = "backstop.ran";
const LOG: &str = "backstop.log";
/// Where a data folder kept all this before it was one per user (`<data>/awake/`).
const LEGACY: &str = "awake";
/// How long a look waits for another process's to finish.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// The longest lid mode keeps sleep off in one go (the backstop's shell has it in minutes: `-mmin +720`).
const CAP: Duration = Duration::from_secs(12 * 3600);
/// After a failed `pmset`, the next try waits 2 s, then twice as long each time, up to this.
const BACKOFF_MAX: Duration = Duration::from_secs(60);

pub const SUDOERS: &str = "/etc/sudoers.d/filo";
const SUDO: &str = "/usr/bin/sudo";
const PMSET: &str = "/usr/bin/pmset";
/// The backstop agent's label (with a suffix of its own for a non-default data folder: launch_agent.rs `label_for`).
const AGENT: &str = "im.filo.desktop.lid-backstop";
/// How often launchd runs the backstop, in seconds.
const BACKSTOP_EVERY: u32 = 60;

/// This user's: `~/Library/Application Support/Filo/awake`.
pub fn shared_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Library/Application Support"))
        .join("Filo/awake")
}

/// Whether this run of Filo may use lid mode: not one with a data folder of its own, unless asked to.
fn allowed() -> bool {
    crate::launch_agent::wanted()
}

use filo_local::lock::{self, flock, would_block};

// ---------------------------------------------------------------------------------------------------------------
// Holders: the processes holding the keep-awake assertion with the option on

/// This process holds the keep-awake assertion with the option on for its data: its file in the shared folder, naming
/// the data folder, locked until it's dropped (or the process ends, however it ends).
pub(super) struct Holder {
    path: PathBuf,
    _file: File,
}

impl Holder {
    pub(super) fn register(dir: &Path, root: &Path) -> Option<Holder> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!("holder-{}.lock", std::process::id()));
        let mut file = lock::claim(&path, 20).ok()??;
        // Which data it holds for; writing it also makes the file new for `holders`' age check.
        file.set_len(0).ok()?;
        file.write_all(root.as_os_str().as_bytes()).ok()?;
        Some(Holder { path, _file: file })
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        // The file goes, then its lock with the descriptor.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn holder_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(files) = std::fs::read_dir(dir) else { return Vec::new() };
    files
        .flatten()
        .map(|file| file.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("holder-") && name.ends_with(".lock"))
        })
        .collect()
}

/// How many processes hold the keep-awake assertion with the option on now, but those holding for `discount` (a data
/// folder whose option was just read off); files left by processes killed outright go.
pub(super) fn holders(dir: &Path, discount: Option<&Path>) -> usize {
    let mut count = 0;
    for path in holder_files(dir) {
        let Ok(file) = File::open(&path) else { continue };
        if lock::look(&file, &path) {
            let held_for = std::fs::read(&path).unwrap_or_default();
            if discount.is_none_or(|root| root.as_os_str().as_bytes() != held_for.as_slice()) {
                count += 1;
            }
        }
    }
    count
}

// ---------------------------------------------------------------------------------------------------------------
// The journal, the last failure and the look

/// What Filo changed: there only while it has sleep disabled (the backstop's shell reads its presence alone).
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Journal {
    sleep_disabled_by_us: bool,
    /// When sleep went off (ms since 1970; kept while it's set again), and by which process.
    since: f64,
    pid: u32,
}

/// The journal, if there is one. One that can't be read says Filo disabled sleep, since an unknown time: setting sleep
/// back on is always safe.
fn read_journal(dir: &Path) -> Option<Journal> {
    let bytes = std::fs::read(dir.join(JOURNAL)).ok()?;
    match serde_json::from_slice::<Journal>(&bytes) {
        Ok(journal) => journal.sleep_disabled_by_us.then_some(journal),
        Err(_) => Some(Journal { sleep_disabled_by_us: true, since: 0.0, pid: 0 }),
    }
}

fn journal_dirty(dir: &Path) -> bool {
    read_journal(dir).is_some()
}

fn write_journal(dir: &Path, since: f64) -> Result<(), String> {
    let journal = Journal { sleep_disabled_by_us: true, since, pid: std::process::id() };
    let bytes = serde_json::to_vec(&journal).map_err(|error| error.to_string())?;
    crate::config::write_atomic(&dir.join(JOURNAL), &bytes)
        .map_err(|error| format!("Couldn't write the journal: {error}"))
}

fn clear_journal(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(JOURNAL));
}

/// The last `pmset` that failed, until one works.
#[derive(Debug, Default, Deserialize, Serialize)]
struct Problem {
    error: String,
    /// How many in a row, and when the last one was (ms since 1970).
    failures: u32,
    at: f64,
}

fn read_problem(dir: &Path) -> Option<Problem> {
    serde_json::from_slice(&std::fs::read(dir.join(PROBLEM)).ok()?).ok()
}

fn record_failure(dir: &Path, error: &str) {
    let failures = read_problem(dir).map_or(0, |problem| problem.failures).saturating_add(1);
    let problem = Problem { error: error.to_string(), failures, at: now_ms() as f64 };
    if let Ok(bytes) = serde_json::to_vec(&problem) {
        let _ = crate::config::write_atomic(&dir.join(PROBLEM), &bytes);
    }
}

fn clear_problem(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(PROBLEM));
}

/// Whether the last failure was too recent to try again: 2 s after the first, doubling up to `BACKOFF_MAX` (a failing
/// `sudo -n` logs an incident each time).
fn backing_off(dir: &Path, now: f64) -> bool {
    let Some(problem) = read_problem(dir) else { return false };
    let wait =
        (2_000f64 * 2f64.powi(problem.failures.saturating_sub(1).min(16) as i32)).min(BACKOFF_MAX.as_millis() as f64);
    now < problem.at + wait
}

/// The Mac, as a look sees it and changes it (a stand-in in tests).
pub(super) trait Machine {
    fn on_ac(&self) -> bool;
    fn critical(&self) -> bool;
    /// Whether sleep is disabled now (`pmset -g`'s SleepDisabled); `None` when that can't be read.
    fn sleep_disabled(&self) -> Option<bool>;
    fn set_sleep_disabled(&self, on: bool) -> Result<(), String>;
    /// The lid is closed and that sleeps the Mac (no external display keeps it going).
    fn lid_closed_sleeps(&self) -> bool;
    fn sleep_now(&self);
    /// Runs `script` as root with macOS's administrator prompt; `Err("cancelled")` when the user cancels it.
    fn as_admin(&self, script: &str, prompt: &str) -> Result<(), String>;
    fn rule_exists(&self) -> bool;
    fn remove_backstop(&self) -> Result<(), String>;
}

/// What a look does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Step {
    Nothing,
    /// `disablesleep 1`, journaled first.
    Disable,
    /// `disablesleep 0`, then the journal goes.
    Restore,
}

/// `want`: a hold with the option on, AC power, not critically hot, not capped. `dirty`: the journal says Filo disabled
/// sleep. `disabled_now`: whether it is, read only while wanted.
fn decide(want: bool, dirty: bool, disabled_now: Option<bool>) -> Step {
    match (want, dirty) {
        // Disabled already, not by Filo: the user's own, left alone. Not known: not touched either.
        (true, false) if disabled_now == Some(false) => Step::Disable,
        // The journal says so but sleep is on (a `pmset` that failed after the journal was written, or someone set it
        // back): again.
        (true, true) if disabled_now == Some(false) => Step::Disable,
        (false, true) => Step::Restore,
        _ => Step::Nothing,
    }
}

/// The lock that makes each process's look one at a time; `None` when another look held it for longer than `wait`.
fn take_lock(dir: &Path, wait: Duration) -> Option<File> {
    std::fs::create_dir_all(dir).ok()?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK)).ok()?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        match flock(&file, libc::LOCK_EX | libc::LOCK_NB) {
            Ok(()) => return Some(file),
            Err(error) if would_block(&error) && std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => return None,
        }
    }
}

/// The process looking, when it has data of its own: its folder, and its option, read under the lock.
pub(super) struct Mine<'a> {
    pub root: &'a Path,
    pub option: &'a dyn Fn() -> bool,
}

/// One look: sets sleep disabled or back on as the holds, the power source, the thermal state and the time it's been
/// off call for. `may_disable` false (the backstop): it only ever sets sleep back on.
pub(super) fn reconcile(dir: &Path, mine: Option<Mine>, may_disable: bool, machine: &dyn Machine) -> Step {
    // Nothing to do and nothing to undo: no lock, no look at the Mac.
    if !dir.join(JOURNAL).exists() && !dir.join(CAPPED).exists() && holder_files(dir).is_empty() {
        return Step::Nothing;
    }
    let Some(_lock) = take_lock(dir, LOCK_WAIT) else { return Step::Nothing };
    let journal = read_journal(dir);
    let dirty = journal.is_some();
    // The option as it is now: a change made while this waited for the lock counts.
    let discount = mine.filter(|mine| !(mine.option)()).map(|mine| mine.root);
    let count = holders(dir, discount);
    let now = now_ms() as f64;
    if count == 0 {
        let _ = std::fs::remove_file(dir.join(CAPPED));
    } else if journal
        .as_ref()
        .is_some_and(|journal| journal.since > 0.0 && now - journal.since >= CAP.as_millis() as f64)
    {
        let _ = std::fs::write(dir.join(CAPPED), b"");
    }
    let capped = dir.join(CAPPED).exists();
    let (on_ac, critical) = (machine.on_ac(), machine.critical());
    let want = count > 0 && on_ac && !critical && !capped;
    let disabled_now = if want { machine.sleep_disabled() } else { None };
    let step = match decide(want, dirty, disabled_now) {
        Step::Disable if !may_disable => Step::Nothing,
        step => step,
    };
    if step != Step::Nothing && backing_off(dir, now) {
        return Step::Nothing;
    }
    match step {
        Step::Disable => {
            let since = journal.as_ref().map(|journal| journal.since).filter(|since| *since > 0.0).unwrap_or(now);
            if let Err(error) = write_journal(dir, since) {
                say(&error);
                return Step::Nothing;
            }
            if let Err(error) = machine.set_sleep_disabled(true) {
                // A failing command may still have changed the setting. Keep the undo unless we can confirm sleep
                // is enabled; an unreadable state must never lose crash recovery.
                if !dirty && machine.sleep_disabled() == Some(false) {
                    clear_journal(dir);
                }
                let error = format!("Couldn't keep the Mac awake with the lid closed: {error}");
                record_failure(dir, &error);
                say(&error);
                return Step::Nothing;
            }
            clear_problem(dir);
            say("lid: sleep disabled while tasks run (plugged in)");
        }
        Step::Restore => match machine.set_sleep_disabled(false) {
            Ok(()) => {
                clear_journal(dir);
                clear_problem(dir);
                let why = match () {
                    _ if capped => "it had been off for 12 hours",
                    _ if count == 0 => "no task holds it off",
                    _ if !on_ac => "on battery",
                    _ => "too hot",
                };
                say(&format!("lid: sleep back on ({why})"));
                // The lid closed meanwhile: sleep now, as closing it would have.
                if machine.lid_closed_sleeps() {
                    machine.sleep_now();
                }
            }
            Err(error) => {
                let error = format!("Couldn't set sleep back on: {error}");
                record_failure(dir, &error);
                say(&format!("{error} (tried again in a while)"));
            }
        },
        Step::Nothing => {}
    }
    step
}

/// A data folder's journal from before it was one per user (`<data>/awake/lid.json`) becomes the shared one, once the
/// processes still running that version (holding files there) have ended: they set sleep back on themselves.
fn migrate_legacy(legacy: &Path, dir: &Path) {
    if legacy == dir || !legacy.join(JOURNAL).exists() {
        return;
    }
    let Some(_legacy_lock) = take_lock(legacy, Duration::from_millis(500)) else { return };
    if !journal_dirty(legacy) || holders(legacy, None) > 0 {
        return;
    }
    let Some(_lock) = take_lock(dir, LOCK_WAIT) else { return };
    if !journal_dirty(dir) && write_journal(dir, now_ms() as f64).is_err() {
        return;
    }
    clear_journal(legacy);
}

/// Before turning lid mode off for good: an old version's journal in the data folder must be gone first, or its process
/// couldn't set sleep back on once the rule is removed. Those processes read the option off within seconds and undo it
/// themselves; one that ended without doing so has its journal moved to the shared one.
fn settle_legacy(legacy: &Path, dir: &Path) -> Result<(), String> {
    for _ in 0..40 {
        migrate_legacy(legacy, dir);
        if !journal_dirty(legacy) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("An older Filo process still has sleep off. Try again once it has seen the change (a few seconds), or after its task ends.".into())
}

/// A line in the log, once while it's the same (a look every couple of seconds would repeat a failure otherwise).
fn say(line: &str) {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if last.as_deref() != Some(line) {
        eprintln!("{line}");
        *last = Some(line.to_string());
    }
}

/// Turning lid mode off for good (the option off, "Remove access", `filo --uninstall-lid`): sleep back on, then the
/// backstop and the rule, all under the look's lock so no look disables sleep in between. `root`: the data folder whose
/// option is off now (its holders don't count). Never removes anything while sleep is still off for another Filo's
/// tasks; if `sudo -n` can't set it back on, one administrator prompt does that and removes the rule.
fn remove_access(dir: &Path, root: Option<&Path>, machine: &dyn Machine) -> Result<(), String> {
    let _lock = take_lock(dir, Duration::from_secs(15))
        .ok_or("Another Filo process is changing sleep right now. Try again in a moment.")?;
    let mut rule_gone = false;
    if journal_dirty(dir) {
        if holders(dir, root) > 0 {
            return Err(
                "Another Filo still has tasks keeping sleep off with the lid closed. Its access stays until they end."
                    .into(),
            );
        }
        if machine.set_sleep_disabled(false).is_err() {
            let prompt = "Filo wants to turn sleep back on and remove the permission it used to keep your Mac awake with the lid closed.";
            match machine.as_admin(&format!("{PMSET} -a disablesleep 0 && /bin/rm -f {SUDOERS}"), prompt) {
                Ok(()) => rule_gone = true,
                Err(error) if error == "cancelled" => return Err("Sleep is still off: the password prompt was cancelled. The backstop keeps trying every minute; use “Remove access” to try again.".into()),
                Err(error) => return Err(format!("Couldn't turn sleep back on: {error}")),
            }
        }
        clear_journal(dir);
    }
    clear_problem(dir);
    let _ = std::fs::remove_file(dir.join(CAPPED));
    let mut problems = Vec::new();
    if let Err(error) = machine.remove_backstop() {
        problems.push(error);
    }
    if !rule_gone && machine.rule_exists() {
        let prompt = "Filo wants to remove the permission it used to keep your Mac awake with the lid closed.";
        match machine.as_admin(&format!("/bin/rm -f {SUDOERS}"), prompt) {
            Ok(()) => {}
            Err(error) if error == "cancelled" => problems.push(format!(
                "The permission stays in {SUDOERS} (the password prompt was cancelled); it's unused while this is off."
            )),
            Err(error) => problems.push(format!("Couldn't remove {SUDOERS}: {error}")),
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join(" ")) }
}

/// What's wrong with lid mode now, for the Coding page: the last failure, the cap reached, or a backstop that's
/// installed (`backstop`) but doesn't run.
fn status(dir: &Path, backstop: bool) -> Option<String> {
    if let Some(problem) = read_problem(dir) {
        return Some(problem.error);
    }
    if dir.join(CAPPED).exists() {
        return Some(
            "Sleep came back on after 12 hours with the lid closed. It stays on until these tasks end.".into(),
        );
    }
    if backstop {
        let ran = std::fs::metadata(dir.join(RAN)).and_then(|meta| meta.modified()).ok();
        let late = Duration::from_secs(5 * BACKSTOP_EVERY as u64);
        return match ran {
            None => Some("The backstop that turns sleep back on if Filo stops hasn't run. Turn this off and on again to set it up.".into()),
            Some(at) if at.elapsed().is_ok_and(|elapsed| elapsed > late) => {
                Some(format!("The backstop that turns sleep back on if Filo stops hasn't run since {}.", crate::format::short_date_time(chrono::DateTime::<chrono::Local>::from(at).timestamp_millis() as f64)))
            }
            Some(_) => None,
        };
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// This Mac

pub(super) struct Mac;

type CFTypeRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(string: CFTypeRef, buffer: *mut libc::c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

/// `NSProcessInfoThermalStateCritical`.
const THERMAL_CRITICAL: isize = 3;

/// Whether the Mac runs on AC power now (slept.rs says so when a lid closed on battery).
pub fn on_ac_power() -> bool {
    Mac.on_ac()
}

impl Machine for Mac {
    fn on_ac(&self) -> bool {
        // SAFETY: the snapshot is released after; the type string belongs to it (the Get rule) and is read before.
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return false;
            }
            let kind = IOPSGetProvidingPowerSourceType(snapshot);
            let mut buffer = [0 as libc::c_char; 64];
            let read = !kind.is_null()
                && CFStringGetCString(kind, buffer.as_mut_ptr(), buffer.len() as isize, 0x0800_0100) != 0;
            CFRelease(snapshot);
            read && std::ffi::CStr::from_ptr(buffer.as_ptr()).to_bytes() == b"AC Power"
        }
    }

    fn critical(&self) -> bool {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        // SAFETY: read-only Foundation messages, fine on any thread.
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            let state: isize = msg_send![info, thermalState];
            state >= THERMAL_CRITICAL
        }
    }

    fn sleep_disabled(&self) -> Option<bool> {
        let output = Command::new(PMSET)
            .arg("-g")
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        Some(sleep_disabled_in(&String::from_utf8_lossy(&output.stdout)))
    }

    fn set_sleep_disabled(&self, on: bool) -> Result<(), String> {
        let output = Command::new(SUDO)
            .args(["-n", PMSET, "-a", "disablesleep", if on { "1" } else { "0" }])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("sudo: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!("sudo pmset: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }

    fn lid_closed_sleeps(&self) -> bool {
        Command::new("/usr/sbin/ioreg")
            .args(["-r", "-k", "AppleClamshellState", "-d", "1"])
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|output| lid_closed_sleeps_in(&String::from_utf8_lossy(&output.stdout)))
    }

    fn sleep_now(&self) {
        // No root needed.
        let _ = Command::new(PMSET).arg("sleepnow").stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    fn as_admin(&self, script: &str, prompt: &str) -> Result<(), String> {
        as_admin(script, prompt)
    }

    fn rule_exists(&self) -> bool {
        Path::new(SUDOERS).exists()
    }

    fn remove_backstop(&self) -> Result<(), String> {
        remove_backstop()
    }
}

/// `pmset -g`'s "SleepDisabled 1" (the line is there only while it's set).
fn sleep_disabled_in(settings: &str) -> bool {
    settings.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("SleepDisabled") && words.next() == Some("1")
    })
}

/// From `ioreg -r -k AppleClamshellState -d 1`: the lid is closed, and closing it sleeps the Mac.
fn lid_closed_sleeps_in(ioreg: &str) -> bool {
    let says = |key: &str| ioreg.lines().any(|line| line.trim() == format!("\"{key}\" = Yes"));
    says("AppleClamshellState") && says("AppleClamshellCausesSleep")
}

// ---------------------------------------------------------------------------------------------------------------
// The rule

/// The rule: exactly the two commands, for this user, nothing else.
fn sudoers_text(user: &str) -> String {
    format!(
        "# Installed by Filo (\"Keep running with the lid closed\" on the Coding page). Exactly two commands, nothing else.\n\
{user} ALL=(root) NOPASSWD: {PMSET} -a disablesleep 1\n\
{user} ALL=(root) NOPASSWD: {PMSET} -a disablesleep 0\n"
    )
}

/// What runs as root, once, to install the rule: written to a file of root's own (nothing of the user's that another
/// process could swap in between), checked with `visudo -c`, then installed root's and read-only.
fn install_script(user: &str) -> String {
    format!(
        "set -e\n\
t=$(/usr/bin/mktemp /tmp/filo-sudoers.XXXXXX)\n\
trap '/bin/rm -f \"$t\"' EXIT\n\
/bin/cat > \"$t\" <<'FILO_SUDOERS'\n{}FILO_SUDOERS\n\
/usr/sbin/visudo -cf \"$t\" >/dev/null\n\
/usr/bin/install -m 0440 -o root -g wheel \"$t\" {SUDOERS}\n",
        sudoers_text(user)
    )
}

/// This user's login name, as sudoers matches it; refused unless it's plain (letters, digits, `.`, `_`, `-`).
fn user_name() -> Result<String, String> {
    // SAFETY: getpwuid's record is read at once, on this thread.
    let name = unsafe {
        let record = libc::getpwuid(libc::getuid());
        if record.is_null() || (*record).pw_name.is_null() {
            return Err("Couldn't tell who's signed in to this Mac.".into());
        }
        std::ffi::CStr::from_ptr((*record).pw_name).to_string_lossy().into_owned()
    };
    plain_name(&name)
        .then_some(name.clone())
        .ok_or_else(|| format!("Your account's name ({name}) can't go in a sudoers rule as it is."))
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Whether the rule is there and lets this user run both commands without a password: both are in what `sudo -n -l`
/// lists as NOPASSWD (`sudo -l <command>` alone would say yes to an administrator's password-protected `ALL` too).
pub(super) fn rule_installed() -> bool {
    if !Path::new(SUDOERS).exists() {
        return false;
    }
    let Ok(output) = Command::new(SUDO).args(["-n", "-l"]).stdin(Stdio::null()).stderr(Stdio::null()).output() else {
        return false;
    };
    lists_both_commands(&String::from_utf8_lossy(&output.stdout))
}

/// Installation may have succeeded only partly, or removal may have been cancelled. Keep cleanup available in the
/// Coding page even when the option itself is off.
pub(super) fn access_installed() -> bool {
    Path::new(SUDOERS).exists() || agent_plist_path().exists()
}

fn lists_both_commands(listing: &str) -> bool {
    let passwordless = |value: &str| {
        let command = format!("{PMSET} -a disablesleep {value}");
        listing.lines().any(|line| {
            line.contains("NOPASSWD:")
                && line
                    .split("NOPASSWD:")
                    .nth(1)
                    .is_some_and(|commands| commands.split(',').any(|listed| listed.trim() == command))
        })
    };
    passwordless("1") && passwordless("0")
}

/// Runs `script` as root with macOS's administrator prompt.
fn as_admin(script: &str, prompt: &str) -> Result<(), String> {
    if crate::test_run::refuse("administrator prompt") {
        return Err("no administrator prompt in test runs".into());
    }
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with prompt (item 2 of argv) with administrator privileges",
            "-e",
            "end run",
            script,
            prompt,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("osascript: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    if said.contains("-128") {
        return Err("cancelled".into());
    }
    Err(said.trim().to_string())
}

// ---------------------------------------------------------------------------------------------------------------
// The backstop

/// What launchd runs every minute and at login: `$1` Filo's binary, `$2` the code requirement pinned when it was
/// installed, `$3` the shared folder, `$4` the data folder (for an old journal there). A binary that passes the check
/// does the look (`backstop_main`); for one that doesn't, or can't be checked (launchd's `codesign` can't read a build
/// inside Desktop or Documents: "Operation not permitted"), none of its code runs and the undo, which is
/// always safe, is done here: when the journal is there and the Mac is on battery, it's been 12 hours (`CAP`) or no
/// hold's file is locked, under the same lock the app's looks take.
const BACKSTOP_SH: &str = r#"bin=$1 req=$2 dir=$3 root=$4
/usr/bin/touch "$dir/backstop.ran" || exit 1
if /usr/bin/codesign --verify --strict -R "=$req" "$bin" 2>/dev/null; then
  "$bin" --lid-backstop --dir "$dir" --root "$root" && exit 0
fi
[ -e "$dir/lid.json" ] || exit 0
exec /usr/bin/lockf -k -t 10 "$dir/lid.lock" /bin/sh -c '
  [ -e "$1/lid.json" ] || exit 0
  if ! /usr/bin/pmset -g ps | /usr/bin/head -1 | /usr/bin/grep -q "AC Power"; then
    why="on battery"
  elif [ -n "$(/usr/bin/find "$1/lid.json" -mmin +720)" ]; then
    why="off for 12 hours"
  else
    for holder in "$1"/holder-*.lock; do
      [ -e "$holder" ] || continue
      /usr/bin/lockf -k -n -s -t 0 "$holder" /usr/bin/true
      [ $? = 75 ] && exit 0
    done
    why="no task holds it off"
  fi
  echo "$(/bin/date): $2 could not be checked or run; setting sleep back on without it ($why)"
  /usr/bin/sudo -n /usr/bin/pmset -a disablesleep 0 || exit 1
  /bin/rm -f "$1/lid.json"
  lid=$(/usr/sbin/ioreg -r -k AppleClamshellState -d 1)
  case $lid in *"\"AppleClamshellState\" = Yes"*) case $lid in *"\"AppleClamshellCausesSleep\" = Yes"*) /usr/bin/pmset sleepnow ;; esac ;; esac
' filo-lid-undo "$dir" "$bin"
"#;

fn agent_label() -> String {
    crate::launch_agent::label_for(AGENT)
}

fn agent_plist_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents").join(format!("{}.plist", agent_label()))
}

/// The binary's designated code requirement (`codesign -d -r-`): its identifier and signer for a signed build, its
/// exact hash (`cdhash`) for an ad-hoc one.
fn requirement(exe: &Path) -> Result<String, String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(exe)
        .output()
        .map_err(|error| format!("codesign: {error}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    text.lines()
        .find_map(|line| line.split_once("designated => ").map(|(_, requirement)| requirement.trim().to_string()))
        .filter(|requirement| !requirement.is_empty())
        .ok_or_else(|| format!("{} has no code signature to check the backstop against.", exe.display()))
}

fn agent_plist(label: &str, exe: &Path, requirement: &str, dir: &Path, root: &Path) -> String {
    use crate::launch_agent::xml_escape;
    let args = [
        "/bin/sh",
        "-c",
        BACKSTOP_SH,
        "filo-lid-backstop",
        &exe.display().to_string(),
        requirement,
        &dir.display().to_string(),
        &root.display().to_string(),
    ];
    let args: String = args.iter().map(|arg| format!("\t\t<string>{}</string>\n", xml_escape(arg))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{args}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>StartInterval</key>
	<integer>{BACKSTOP_EVERY}</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        log = xml_escape(&dir.join(LOG).display().to_string()),
    )
}

fn agent_target() -> String {
    format!("{}/{}", crate::launch_agent::domain(), agent_label())
}

fn agent_loaded() -> bool {
    Command::new("/bin/launchctl")
        .args(["print", &agent_target()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Installs the backstop, or pins it to this build (a rebuild or an update changes the requirement); then has it run
/// once and waits for it to have run, so a backstop that can't is known now.
fn install_backstop(dir: &Path, root: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("Where Filo is: {error}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    std::fs::create_dir_all(dir).map_err(|error| format!("Couldn't make {}: {error}", dir.display()))?;
    let content = agent_plist(&agent_label(), &exe, &requirement(&exe)?, dir, root);
    let path = agent_plist_path();
    let loaded = agent_loaded();
    let started = SystemTime::now();
    if !(loaded && std::fs::read_to_string(&path).is_ok_and(|current| current == content)) {
        crate::config::write_atomic(&path, content.as_bytes())
            .map_err(|error| format!("Couldn't write {}: {error}", path.display()))?;
        if loaded {
            let _ = crate::launch_agent::launchctl(&["bootout", &agent_target()]);
        }
        // Booting out finishes in the background: bootstrap can fail for a moment after it.
        let mut last = Ok(());
        for _ in 0..10 {
            last = crate::launch_agent::launchctl(&[
                "bootstrap",
                &crate::launch_agent::domain(),
                &path.display().to_string(),
            ]);
            if last.is_ok() || agent_loaded() {
                last = Ok(());
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        last?;
    }
    // Run it now (it may be running from loading already) and see that it did.
    let _ = crate::launch_agent::launchctl(&["kickstart", &agent_target()]);
    let ran = || {
        std::fs::metadata(dir.join(RAN))
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at >= started - Duration::from_secs(1))
    };
    for _ in 0..50 {
        if ran() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("launchd didn't run it (see {})", dir.join(LOG).display()))
}

fn remove_backstop() -> Result<(), String> {
    if agent_loaded() {
        crate::launch_agent::launchctl(&["bootout", &agent_target()])?;
    }
    match std::fs::remove_file(agent_plist_path()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("Couldn't remove {}: {error}", agent_plist_path().display()))
        }
        _ => Ok(()),
    }
}

fn arg_after(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).map(PathBuf::from)
}

/// `filo --lid-backstop [--dir <shared folder>] [--root <data folder>]`: one look that only ever sets sleep back on
/// (what the backstop agent runs once the binary passes its signature check). It reads no option: only holders count.
pub fn backstop_main(args: &[String]) -> i32 {
    let dir = arg_after(args, "--dir").unwrap_or_else(shared_dir);
    let _ = std::fs::create_dir_all(&dir);
    let _ = File::create(dir.join(RAN));
    if let Some(root) = arg_after(args, "--root") {
        migrate_legacy(&root.join(LEGACY), &dir);
    }
    if reconcile(&dir, None, false, &Mac) == Step::Restore && !journal_dirty(&dir) {
        eprintln!("{}: the backstop set sleep back on", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
    0
}

/// `filo --uninstall-lid`: for deleting Filo, everything lid mode installed goes: sleep back on, the option off, the
/// backstop and (with the administrator prompt) the rule.
pub fn uninstall_main() -> i32 {
    let root = crate::config::config_dir().join("local");
    let dir = shared_dir();
    if let Ok(db) = filo_local::db::Db::open(&root.join(filo_local::db::FILE)) {
        let _ = filo_local::coding::set_keep_awake_lid(&db, false);
    }
    match settle_legacy(&root.join(LEGACY), &dir).and_then(|()| remove_access(&dir, Some(&root), &Mac)) {
        Ok(()) => {
            println!("Lid mode is off: sleep is on, and the backstop and {SUDOERS} are gone.");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

/// Both switches on: tasks keep the Mac awake, and with the lid closed too.
fn option_on(db: &filo_local::db::Db) -> bool {
    filo_local::coding::keep_awake(db).unwrap_or(true) && filo_local::coding::keep_awake_lid(db).unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------------------------
// The app's side

impl Local {
    /// Where lid mode's state is: this user's (each test's data of its own).
    pub(super) fn lid_dir(&self) -> PathBuf {
        if cfg!(test) { self.root.join("lid") } else { shared_dir() }
    }

    pub(super) fn lid_option(&self) -> bool {
        allowed() && self.db().is_ok_and(option_on)
    }

    /// One look from this process (awake.rs, every couple of seconds while it holds, and as its last hold ends).
    pub(super) fn lid_reconcile(&self) {
        let dir = self.lid_dir();
        migrate_legacy(&self.root.join(LEGACY), &dir);
        reconcile(&dir, Some(Mine { root: &self.root, option: &|| self.lid_option() }), true, &Mac);
    }

    /// What's wrong with lid mode, for the Coding page, while it's on or its access is installed.
    pub(super) fn lid_status(&self, on: bool, access: bool) -> Option<String> {
        (on || access).then(|| status(&self.lid_dir(), agent_plist_path().exists())).flatten()
    }

    /// `coding:setKeepAwakeLid`. On: the rule (asking for the password the first time), the backstop, then the option.
    /// Off: the option, sleep back on, the backstop, then the rule (asking for the password again).
    pub(super) fn set_keep_awake_lid(&self, on: bool) -> Result<Value, String> {
        let result = self.change_keep_awake_lid(on);
        // Installed access can change even on an error (a cancelled removal, or failed backstop setup).
        self.changed();
        result
    }

    fn change_keep_awake_lid(&self, on: bool) -> Result<Value, String> {
        let db = self.db()?;
        let dir = self.lid_dir();
        if on {
            if !allowed() {
                return Err("Lid mode is off for a Filo with a data folder of its own (FILO_CONFIG_DIR); set FILO_LAUNCH_AGENT=1 to use it.".into());
            }
            if !rule_installed() {
                let prompt = "Filo wants to keep your coding tasks running while the lid is closed. This lets it switch sleep off and back on (pmset disablesleep) without asking again, and nothing else.";
                match as_admin(&install_script(&user_name()?), prompt) {
                    Ok(()) => {}
                    Err(error) if error == "cancelled" => {
                        return Err("Nothing changed: the password prompt was cancelled.".into());
                    }
                    Err(error) => return Err(format!("Couldn't install the permission: {error}")),
                }
                if !rule_installed() {
                    return Err(format!("{SUDOERS} was installed, but sudo doesn't allow it yet. Try again."));
                }
            }
            install_backstop(&dir, &self.root).map_err(|error| {
                format!("Couldn't set up the backstop that turns sleep back on if Filo stops: {error}")
            })?;
            filo_local::coding::set_keep_awake_lid(db, true)?;
            clear_problem(&dir);
            // This process's holder follows the option, and a look.
            self.keep_awake_changed();
            return Ok(json!(true));
        }
        filo_local::coding::set_keep_awake_lid(db, false)?;
        // This process's holder goes, and a look sets sleep back on: holders for this data don't count from now on.
        self.keep_awake_changed();
        settle_legacy(&self.root.join(LEGACY), &dir)?;
        remove_access(&dir, Some(&self.root), &Mac)?;
        Ok(json!(false))
    }

    /// When the app opens: the backstop pinned to this build (while the option is on), and one look, which sets sleep
    /// back on if a previous run left it off with nothing running.
    pub(super) fn lid_at_launch(self: &Arc<Self>) {
        let local = self.clone();
        std::thread::spawn(move || {
            if local.lid_option()
                && let Err(error) = install_backstop(&local.lid_dir(), &local.root)
            {
                eprintln!("lid: the backstop couldn't be pinned to this build: {error}");
            }
            local.lid_reconcile();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Fake {
        battery: Cell<bool>,
        hot: Cell<bool>,
        disabled: Cell<bool>,
        fails: Cell<bool>,
        changes_before_failure: bool,
        unknown_after_failure: bool,
        lid_closed: Cell<bool>,
        calls: RefCell<Vec<bool>>,
        slept: Cell<u32>,
        rule: Cell<bool>,
        admin: RefCell<Vec<String>>,
        admin_cancels: bool,
        backstop_removed: Cell<bool>,
    }

    impl Machine for Fake {
        fn on_ac(&self) -> bool {
            !self.battery.get()
        }
        fn critical(&self) -> bool {
            self.hot.get()
        }
        fn sleep_disabled(&self) -> Option<bool> {
            (!self.unknown_after_failure || self.calls.borrow().is_empty()).then(|| self.disabled.get())
        }
        fn set_sleep_disabled(&self, on: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(on);
            if self.fails.get() {
                if self.changes_before_failure {
                    self.disabled.set(on);
                }
                return Err("sudo: a password is required".into());
            }
            self.disabled.set(on);
            Ok(())
        }
        fn lid_closed_sleeps(&self) -> bool {
            self.lid_closed.get()
        }
        fn sleep_now(&self) {
            self.slept.set(self.slept.get() + 1);
        }
        fn as_admin(&self, script: &str, _prompt: &str) -> Result<(), String> {
            self.admin.borrow_mut().push(script.to_string());
            if self.admin_cancels {
                return Err("cancelled".into());
            }
            if script.contains("disablesleep 0") {
                self.disabled.set(false);
            }
            if script.contains(SUDOERS) {
                self.rule.set(false);
            }
            Ok(())
        }
        fn rule_exists(&self) -> bool {
            self.rule.get()
        }
        fn remove_backstop(&self) -> Result<(), String> {
            self.backstop_removed.set(true);
            Ok(())
        }
    }

    /// A folder of the test's own, removed however the test ends.
    struct Temp(PathBuf);

    impl std::ops::Deref for Temp {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp() -> Temp {
        let root = std::env::temp_dir().join(format!("filo-lid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Temp(root)
    }

    /// The app's look for data at `root` with its option `on`.
    fn look(dir: &Path, root: &Path, on: bool, machine: &dyn Machine) -> Step {
        reconcile(dir, Some(Mine { root, option: &|| on }), true, machine)
    }

    /// Another process's holder (this one can have only one): a file of its name, locked through a descriptor of its own.
    fn other_holder(dir: &Path, root: &Path) -> File {
        let path = dir.join("holder-1.lock");
        std::fs::write(&path, root.as_os_str().as_bytes()).unwrap();
        let file = File::open(&path).unwrap();
        flock(&file, libc::LOCK_EX | libc::LOCK_NB).unwrap();
        file
    }

    /// A failure long enough ago to try again.
    fn forget_backoff(dir: &Path) {
        if let Some(mut problem) = read_problem(dir) {
            problem.at = 0.0;
            std::fs::write(dir.join(PROBLEM), serde_json::to_vec(&problem).unwrap()).unwrap();
        }
    }

    #[test]
    fn decisions() {
        assert_eq!(decide(true, false, Some(false)), Step::Disable);
        assert_eq!(decide(true, false, Some(true)), Step::Nothing, "the user's own");
        assert_eq!(decide(true, false, None), Step::Nothing);
        assert_eq!(decide(true, true, Some(true)), Step::Nothing);
        assert_eq!(decide(true, true, Some(false)), Step::Disable, "journaled but on: again");
        assert_eq!(decide(true, true, None), Step::Nothing);
        assert_eq!(decide(false, true, None), Step::Restore);
        assert_eq!(decide(false, false, None), Step::Nothing);
    }

    /// Disabled only while a hold is active with the option on, on AC power and not critically hot; back on when the
    /// hold ends, on battery, critically hot or with the option off; the backstop only ever sets it back.
    #[test]
    fn sleep_follows_the_holds_the_power_and_the_option() {
        let (dir, root) = (temp(), temp());
        let mac = Fake::default();
        assert_eq!(look(&dir, &root, true, &mac), Step::Nothing, "no hold");
        let holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(holders(&dir, None), 1);
        assert_eq!(reconcile(&dir, None, false, &mac), Step::Nothing, "the backstop never disables");
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable);
        assert!(mac.disabled.get() && journal_dirty(&dir));
        assert_eq!(look(&dir, &root, true, &mac), Step::Nothing, "once");
        drop(holder);
        assert_eq!(holders(&dir, None), 0);
        assert_eq!(look(&dir, &root, true, &mac), Step::Restore, "the last hold ended");
        assert!(!mac.disabled.get() && !journal_dirty(&dir));

        let _holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable);
        mac.battery.set(true);
        assert_eq!(look(&dir, &root, true, &mac), Step::Restore, "on battery");
        mac.battery.set(false);
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable, "plugged in again");
        mac.hot.set(true);
        assert_eq!(look(&dir, &root, true, &mac), Step::Restore, "critically hot");
        mac.hot.set(false);
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable);
        assert_eq!(look(&dir, &root, false, &mac), Step::Restore, "the option went off");
        assert!(!mac.disabled.get());
        // Disabled by the user: left alone, and nothing to undo.
        let theirs = Fake { disabled: Cell::new(true), ..Fake::default() };
        assert_eq!(look(&dir, &root, true, &theirs), Step::Nothing);
        assert!(!journal_dirty(&dir) && theirs.calls.borrow().is_empty());
    }

    /// The option is read under the lock, and turning it off discounts every holder for that data at once (a worker
    /// that read it on before can't keep sleep off); another data folder's holder still counts.
    #[test]
    fn the_option_is_read_under_the_lock_and_discounts_its_data() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (dir, mine, other) = (temp(), temp(), temp());
        let mac = Fake::default();
        let worker = Holder::register(&dir, &mine).unwrap();
        assert_eq!(look(&dir, &mine, true, &mac), Step::Disable);
        // A look starts with the option on and waits for the lock; the user turns it off meanwhile.
        let option = AtomicBool::new(true);
        let lock = take_lock(&dir, LOCK_WAIT).unwrap();
        let step = std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                let mac = Fake { disabled: Cell::new(true), ..Fake::default() };
                (
                    reconcile(&dir, Some(Mine { root: &mine, option: &|| option.load(Ordering::SeqCst) }), true, &mac),
                    mac.disabled.get(),
                )
            });
            std::thread::sleep(Duration::from_millis(200));
            option.store(false, Ordering::SeqCst);
            drop(lock);
            waiting.join().unwrap()
        });
        assert_eq!(step, (Step::Restore, false), "its own data's holders don't count once the option is off");
        assert!(!journal_dirty(&dir));
        // Another Filo's tasks, its option on, still keep sleep off.
        mac.disabled.set(false);
        let theirs = other_holder(&dir, &other);
        assert_eq!(holders(&dir, Some(&mine)), 1);
        assert_eq!(look(&dir, &mine, false, &mac), Step::Disable);
        drop(theirs);
        assert_eq!(look(&dir, &mine, false, &mac), Step::Restore);
        drop(worker);
    }

    /// The backstop's case: the process that disabled sleep was killed outright, leaving its file and the journal.
    #[test]
    fn the_backstop_undoes_what_a_dead_holder_left() {
        let dir = temp();
        let mac = Fake { disabled: Cell::new(true), ..Fake::default() };
        write_journal(&dir, now_ms() as f64).unwrap();
        // Its file is there; nobody holds its lock.
        std::fs::write(dir.join("holder-999999.lock"), b"/somewhere").unwrap();
        assert_eq!(holders(&dir, None), 0);
        assert_eq!(reconcile(&dir, None, false, &mac), Step::Restore);
        assert!(!mac.disabled.get() && !journal_dirty(&dir));
        assert_eq!(*mac.calls.borrow(), [false]);
    }

    /// A failed `pmset` clears the journal only with confirmed sleep enabled, and keeps it when undoing fails; retries
    /// back off; a journal that says off while sleep is on is set again, not left silently not working.
    #[test]
    fn failures_keep_the_journal_true_and_back_off() {
        let (dir, root) = (temp(), temp());
        let _holder = Holder::register(&dir, &root).unwrap();
        let broken = Fake { fails: Cell::new(true), ..Fake::default() };
        assert_eq!(look(&dir, &root, true, &broken), Step::Nothing);
        assert!(!journal_dirty(&dir));
        assert!(
            status(&dir, false).is_some_and(|said| said.starts_with("Couldn't keep the Mac awake")),
            "the Coding page says so"
        );
        assert_eq!(look(&dir, &root, true, &broken), Step::Nothing);
        assert_eq!(broken.calls.borrow().len(), 1, "not again at once");
        forget_backoff(&dir);
        assert_eq!(look(&dir, &root, true, &broken), Step::Nothing);
        assert_eq!(broken.calls.borrow().len(), 2, "again after a while");
        assert!(
            read_problem(&dir).unwrap().failures == 2 && backing_off(&dir, now_ms() as f64 + 3_000.0),
            "longer each time"
        );
        clear_problem(&dir);
        // A command can fail after changing sleep, or leave an unreadable outcome. Both still need recovery.
        for unknown_after_failure in [false, true] {
            let uncertain =
                Fake { fails: Cell::new(true), changes_before_failure: true, unknown_after_failure, ..Fake::default() };
            assert_eq!(look(&dir, &root, true, &uncertain), Step::Nothing);
            assert!(uncertain.disabled.get() && journal_dirty(&dir));
            clear_problem(&dir);
            let recovered = Fake { disabled: Cell::new(true), ..Fake::default() };
            assert_eq!(look(&dir, &root, false, &recovered), Step::Restore);
            assert!(!recovered.disabled.get() && !journal_dirty(&dir));
        }
        // Journaled, but the command didn't take: while still wanted, it's tried again (keeping when it began).
        let lost = Fake { fails: Cell::new(true), ..Fake::default() };
        let began = now_ms() as f64 - 60_000.0;
        write_journal(&dir, began).unwrap();
        assert_eq!(look(&dir, &root, true, &lost), Step::Nothing);
        assert!(journal_dirty(&dir), "kept: it was journaled before");
        lost.fails.set(false);
        forget_backoff(&dir);
        assert_eq!(look(&dir, &root, true, &lost), Step::Disable);
        assert!(lost.disabled.get() && read_journal(&dir).unwrap().since == began && status(&dir, false).is_none());
        lost.fails.set(true);
        assert_eq!(look(&dir, &root, false, &lost), Step::Restore);
        assert!(journal_dirty(&dir), "kept for the next look");
        std::fs::write(dir.join(JOURNAL), b"not json").unwrap();
        assert!(journal_dirty(&dir), "unreadable counts as disabled");
    }

    /// Sleep goes back on after `CAP` even while tasks run (the backstop too), and stays on until they've all ended.
    #[test]
    fn sleep_is_off_twelve_hours_at_most() {
        let (dir, root) = (temp(), temp());
        let mac = Fake::default();
        let holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable);
        write_journal(&dir, now_ms() as f64 - CAP.as_millis() as f64 - 1.0).unwrap();
        assert_eq!(reconcile(&dir, None, false, &mac), Step::Restore, "the backstop too");
        assert!(!mac.disabled.get() && status(&dir, false).is_some_and(|said| said.contains("12 hours")));
        assert_eq!(look(&dir, &root, true, &mac), Step::Nothing, "not again while these tasks run");
        drop(holder);
        assert_eq!(look(&dir, &root, true, &mac), Step::Nothing);
        assert!(!dir.join(CAPPED).exists());
        let _holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable, "new tasks, a new stretch");
        assert!(BACKSTOP_SH.contains(&format!("-mmin +{}", CAP.as_secs() / 60)));
    }

    /// Back on with the lid closed (on battery, say): the Mac sleeps, as closing the lid would have done.
    #[test]
    fn a_closed_lid_sleeps_once_sleep_is_back_on() {
        let (dir, root) = (temp(), temp());
        let mac = Fake::default();
        let _holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(look(&dir, &root, true, &mac), Step::Disable);
        mac.lid_closed.set(true);
        assert_eq!(mac.slept.get(), 0);
        mac.battery.set(true);
        assert_eq!(look(&dir, &root, true, &mac), Step::Restore);
        assert_eq!(mac.slept.get(), 1);
        let ioreg =
            "+-o AppleSMC\n    {\n      \"AppleClamshellCausesSleep\" = Yes\n      \"AppleClamshellState\" = Yes\n";
        assert!(lid_closed_sleeps_in(ioreg));
        assert!(
            !lid_closed_sleeps_in(&ioreg.replace("CausesSleep\" = Yes", "CausesSleep\" = No")),
            "an external display keeps it going"
        );
        assert!(!lid_closed_sleeps_in(&ioreg.replace("State\" = Yes", "State\" = No")));
    }

    /// A stale holder's file goes only while nobody holds it; one being made anew at the same name (a reused pid) is
    /// never lost.
    #[test]
    fn a_reused_holder_file_is_never_lost() {
        let (dir, root) = (temp(), temp());
        let path = dir.join(format!("holder-{}.lock", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        let holder = Holder::register(&dir, &root).unwrap();
        assert_eq!(holders(&dir, None), 1, "fresh, and held");
        assert!(std::fs::metadata(&path).unwrap().modified().unwrap() > old);
        assert_eq!(std::fs::read(&path).unwrap(), root.as_os_str().as_bytes());
        drop(holder);
        assert!(!path.exists());
    }

    /// Turning it off: sleep back on, then the backstop and the rule, with one prompt; if `sudo -n` doesn't work any
    /// more, that one prompt sets sleep back on too. Nothing goes while another Filo's tasks keep sleep off.
    #[test]
    fn removing_access_sets_sleep_back_on_first() {
        let (dir, root, other) = (temp(), temp(), temp());
        // Plain: no journal.
        let mac = Fake { rule: Cell::new(true), ..Fake::default() };
        remove_access(&dir, Some(&root), &mac).unwrap();
        assert!(mac.backstop_removed.get() && !mac.rule.get());
        assert_eq!(*mac.admin.borrow(), [format!("/bin/rm -f {SUDOERS}")]);
        // Nothing installed: no prompt.
        let bare = Fake::default();
        remove_access(&dir, Some(&root), &bare).unwrap();
        assert!(bare.admin.borrow().is_empty());
        // Sleep off and sudo broken: one prompt does both.
        let broken =
            Fake { rule: Cell::new(true), disabled: Cell::new(true), fails: Cell::new(true), ..Fake::default() };
        write_journal(&dir, now_ms() as f64).unwrap();
        let ours = Holder::register(&dir, &root).unwrap();
        remove_access(&dir, Some(&root), &broken).unwrap();
        assert_eq!(*broken.admin.borrow(), [format!("{PMSET} -a disablesleep 0 && /bin/rm -f {SUDOERS}")]);
        assert!(!broken.disabled.get() && !broken.rule.get() && !journal_dirty(&dir) && broken.backstop_removed.get());
        drop(ours);
        // Cancelled: everything stays, so the backstop can still set it back on.
        let cancels = Fake {
            rule: Cell::new(true),
            disabled: Cell::new(true),
            fails: Cell::new(true),
            admin_cancels: true,
            ..Fake::default()
        };
        write_journal(&dir, now_ms() as f64).unwrap();
        assert!(remove_access(&dir, Some(&root), &cancels).is_err());
        assert!(journal_dirty(&dir) && cancels.rule.get() && !cancels.backstop_removed.get());
        // Another Filo's tasks keep it off: nothing goes.
        let theirs = other_holder(&dir, &other);
        let busy = Fake { rule: Cell::new(true), disabled: Cell::new(true), ..Fake::default() };
        assert!(remove_access(&dir, Some(&root), &busy).is_err());
        assert!(
            journal_dirty(&dir) && busy.rule.get() && !busy.backstop_removed.get() && busy.calls.borrow().is_empty()
        );
        drop(theirs);
    }

    /// An old data folder's journal becomes the shared one once nothing of that version holds there any more.
    #[test]
    fn an_old_journal_moves_to_the_shared_folder() {
        let (dir, root) = (temp(), temp());
        let legacy = root.join(LEGACY);
        write_journal(&legacy, now_ms() as f64).unwrap();
        let old = Holder::register(&legacy, &root).unwrap();
        migrate_legacy(&legacy, &dir);
        assert!(journal_dirty(&legacy) && !journal_dirty(&dir), "its process still runs and undoes it itself");
        drop(old);
        migrate_legacy(&legacy, &dir);
        assert!(!journal_dirty(&legacy) && journal_dirty(&dir));
        let mac = Fake { disabled: Cell::new(true), ..Fake::default() };
        assert_eq!(reconcile(&dir, None, false, &mac), Step::Restore);
        // Turning off waits for an old process's journal to go before anything is removed.
        write_journal(&legacy, now_ms() as f64).unwrap();
        let old = Holder::register(&legacy, &root).unwrap();
        let undone = std::thread::scope(|scope| {
            let settling = scope.spawn(|| settle_legacy(&legacy, &dir));
            std::thread::sleep(Duration::from_millis(300));
            clear_journal(&legacy);
            settling.join().unwrap()
        });
        assert!(undone.is_ok() && !journal_dirty(&dir), "it undid it itself");
        drop(old);
    }

    /// The Coding page hears about a backstop that's installed but doesn't run (launchd can't run it, or can't reach
    /// the folder).
    #[test]
    fn a_backstop_that_does_not_run_is_said() {
        let dir = temp();
        assert_eq!(status(&dir, false), None);
        assert!(status(&dir, true).is_some_and(|said| said.contains("hasn't run")));
        let ran = File::create(dir.join(RAN)).unwrap();
        assert_eq!(status(&dir, true), None, "just ran");
        ran.set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();
        assert!(status(&dir, true).is_some_and(|said| said.contains("hasn't run since")));
    }

    /// What `sudo -n -l` lists for an administrator with the rule, and without it.
    #[test]
    fn the_rule_counts_only_as_passwordless_commands() {
        let with = "User me may run the following commands on Mac:\n    (ALL) ALL\n    (root) NOPASSWD: /usr/bin/pmset -a disablesleep 1\n    (root) NOPASSWD: /usr/bin/pmset -a disablesleep 0\n";
        assert!(lists_both_commands(with));
        assert!(
            !lists_both_commands("User me may run the following commands on Mac:\n    (ALL) ALL\n"),
            "an administrator's ALL needs a password"
        );
        assert!(!lists_both_commands("    (root) NOPASSWD: /usr/bin/pmset -a disablesleep 1\n"));
        assert!(!lists_both_commands(
            "    (root) NOPASSWD: /usr/bin/pmset -a disablesleep 10\n    (root) NOPASSWD: /usr/bin/pmset -a disablesleep 0\n"
        ));
    }

    #[test]
    fn pmset_settings_say_whether_sleep_is_disabled() {
        assert!(sleep_disabled_in("System-wide power settings:\n SleepDisabled\t\t1\nCurrently in use:\n standby 1\n"));
        assert!(!sleep_disabled_in(" SleepDisabled\t\t0\n"));
        assert!(!sleep_disabled_in("Currently in use:\n sleep 1\n"));
    }

    /// The rule is exactly the two commands, and `visudo` (which checks a file without root) takes it.
    #[test]
    fn the_rule_is_two_commands_and_valid() {
        let text = sudoers_text("someone");
        let commands: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
        assert_eq!(
            commands,
            [
                "someone ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1",
                "someone ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 0"
            ]
        );
        let root = temp();
        let file = root.join("sudoers");
        std::fs::write(&file, &text).unwrap();
        let checked = Command::new("/usr/sbin/visudo").arg("-cf").arg(&file).output().unwrap();
        assert!(checked.status.success(), "{}", String::from_utf8_lossy(&checked.stderr));
        // The script that installs it carries the same text, and checks it before installing.
        let script = install_script("someone");
        assert!(
            script.contains(&text) && script.find("visudo -cf").unwrap() < script.find("/usr/bin/install").unwrap()
        );
        assert!(plain_name("sam.r_1") && !plain_name("a b") && !plain_name("-x") && !plain_name("a,ALL"));
    }

    /// The plist launchd gets is valid, and runs the shell with the binary, its requirement and the folders.
    #[test]
    fn the_backstop_agent_is_a_valid_plist() {
        let root = temp();
        let path = root.join("agent.plist");
        std::fs::write(
            &path,
            agent_plist(
                "im.filo.test",
                Path::new("/A/filo"),
                "cdhash H\"ab\"",
                &root.join("shared"),
                Path::new("/data"),
            ),
        )
        .unwrap();
        let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        let json = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-"]).arg(&path).output().unwrap();
        let plist: Value = serde_json::from_slice(&json.stdout).unwrap();
        let args: Vec<&str> =
            plist["ProgramArguments"].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap()).collect();
        assert_eq!(&args[..2], ["/bin/sh", "-c"]);
        assert_eq!(
            &args[3..],
            ["filo-lid-backstop", "/A/filo", "cdhash H\"ab\"", &root.join("shared").display().to_string(), "/data"]
        );
        assert_eq!(plist["StartInterval"], 60);
        assert_eq!(plist["RunAtLoad"], true);
        assert!(shared_dir().ends_with("Library/Application Support/Filo/awake"), "outside the folders macOS guards");
    }

    /// Execute the unsigned-build fallback using real macOS lockf, with sudo replaced by a recorder and the power source
    /// by a stand-in. This never changes machine settings or installs access, but checks that its locks agree with the
    /// app's flock locks, and that it sets sleep back on over a live holder on battery.
    #[test]
    fn the_shell_backstop_respects_live_holders_and_the_reconcile_lock() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp();
        let data = root.join("data with spaces");
        let holder = Holder::register(&data, &root).unwrap();
        write_journal(&data, now_ms() as f64).unwrap();
        let recorder = root.join("sudo-recorder");
        let calls = root.join("calls");
        std::fs::write(&recorder, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", calls.display())).unwrap();
        std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let script = |power: &str| {
            BACKSTOP_SH
                .replace("/usr/bin/codesign", "/usr/bin/false")
                .replace("/usr/bin/sudo", &recorder.display().to_string())
                .replace("-t 10", "-t 0")
                .replace("/usr/bin/pmset -g ps", &format!("echo Now drawing from {power}"))
                .replace("/usr/bin/pmset sleepnow", "/usr/bin/true")
        };
        let run = |power: &str| {
            Command::new("/bin/sh")
                .args(["-c", &script(power), "test-backstop", "/missing/filo", "cdhash H\"bad\""])
                .arg(&data)
                .arg("/data")
                .output()
                .unwrap()
        };
        assert!(run("AC Power").status.success());
        assert!(!calls.exists() && journal_dirty(&data), "a live holder must keep its journal");
        assert!(data.join(RAN).exists(), "each run leaves its trace");
        let lock = take_lock(&data, LOCK_WAIT).unwrap();
        assert_eq!(run("Battery Power").status.code(), Some(75), "the app and shell must share a lock");
        assert!(!calls.exists() && journal_dirty(&data));
        drop(lock);
        let output = run("Battery Power");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(!journal_dirty(&data), "on battery, back on whatever holds");
        assert_eq!(std::fs::read_to_string(&calls).unwrap(), "-n /usr/bin/pmset -a disablesleep 0\n");
        drop(holder);
        write_journal(&data, now_ms() as f64).unwrap();
        assert!(run("AC Power").status.success());
        assert!(!journal_dirty(&data), "nothing holds");
    }
}

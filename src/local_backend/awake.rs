// Keeping the Mac from idle sleep while coding tasks, routines and chat replies run: "Keep Mac awake while tasks run"
// on the Coding page (off by default, crates/filo-local `coding::keep_awake`).
//
// Whichever process runs the work keeps the Mac awake: a round's or a turn's worker (local_backend/workers.rs), or the
// app for one it runs itself (a group turn, a round or turn with no worker); the app or the scheduler helper for a
// routine's run. The work takes a `Hold` for as long as it runs. While a process has any and the setting is on, it holds one `PreventUserIdleSystemSleep` power assertion (what
// `caffeinate -i` takes: no root, no private API), named for the work, which `pmset -g assertions` lists under the
// process. The setting is read again every few seconds while work runs, so turning it off lets the Mac sleep again
// without stopping anything (the app's own hold lets go at once). The system drops a process's assertions when it
// ends, so a worker killed outright leaves nothing behind. The display may still sleep, and closing the lid still
// sleeps the Mac, unless "Keep running with the lid closed" is on too (lid.rs): while it holds the assertion with that on,
// a process keeps a holder's file locked, and it looks as work starts (on a thread of its own), every few seconds, and
// as its last hold ends, at whether sleep should be disabled.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::Duration;

use super::Local;
use super::lid::Holder;

/// How often the setting is read again while work runs.
const RECHECK: Duration = Duration::from_secs(2);

/// Work under way in this process: the Mac may idle-sleep again once none is left.
pub struct Hold(u64);

impl Drop for Hold {
    fn drop(&mut self) {
        let local = {
            let mut state = state();
            state.holds.retain(|(id, _)| *id != self.0);
            apply(&mut state);
            state.holds.is_empty().then(|| state.local.upgrade()).flatten()
        };
        // The last one: sleep goes back on now if this process's hold was what kept it off (a worker ends right after).
        if let Some(local) = local {
            local.lid_reconcile();
        }
    }
}

#[derive(Default)]
struct State {
    /// The work under way: its hold, and what it is ("coding task: …"; plain ASCII around the title, as pmset prints
    /// anything else as `?`).
    holds: Vec<(u64, String)>,
    next: u64,
    /// The setting as last read, and "Keep running with the lid closed" with it.
    keep: bool,
    lid: bool,
    /// The assertion held now, and its name.
    held: Option<(u32, String)>,
    /// Where the setting is read: this process's data, and where lid.rs keeps its holders.
    local: Weak<Local>,
    root: Option<PathBuf>,
    lid_dir: Option<PathBuf>,
    /// While the assertion is held with lid mode on, its lock file for lid.rs.
    holder: Option<Holder>,
    /// A thread reads the setting while work runs.
    watching: bool,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Local {
    /// Keeps the Mac awake (while the setting is on) until the returned hold is dropped. `what` names the work in the
    /// assertion.
    pub(super) fn keep_awake(self: &Arc<Self>, what: String) -> Hold {
        let (keep, lid) = self.keep_awake_settings();
        let mut state = state();
        state.next += 1;
        let id = state.next;
        state.holds.push((id, what));
        state.local = Arc::downgrade(self);
        state.root = Some(self.root.clone());
        state.lid_dir = Some(self.lid_dir());
        (state.keep, state.lid) = (keep, lid);
        apply(&mut state);
        if !state.watching {
            state.watching = true;
            if let Err(error) = std::thread::Builder::new().name("filo-awake".into()).spawn(watch) {
                eprintln!("awake: couldn't watch the setting: {error}");
                state.watching = false;
            }
        }
        drop(state);
        // Cover a lid closed immediately after work starts, before the watcher's first recheck; without holding up the
        // work, as a look can wait seconds for another process's.
        if lid {
            let local = self.clone();
            let _ = std::thread::Builder::new().name("filo-lid".into()).spawn(move || local.lid_reconcile());
        }
        Hold(id)
    }

    /// The setting changed (the Coding page): this process's assertion follows at once, the workers' within seconds.
    pub(super) fn keep_awake_changed(&self) {
        let (keep, lid) = self.keep_awake_settings();
        {
            let mut state = state();
            (state.keep, state.lid) = (keep, lid);
            apply(&mut state);
        }
        self.lid_reconcile();
    }

    /// "Keep Mac awake while tasks run", and whether lid mode is on with it (lid.rs `lid_option`).
    fn keep_awake_settings(&self) -> (bool, bool) {
        let keep = self.db().and_then(filo_local::coding::keep_awake).unwrap_or(false);
        (keep, keep && self.lid_option())
    }
}

/// Reads the setting again every few seconds for as long as work runs, and looks at the lid's (lid.rs): the power
/// source and the thermal state change with nothing here changing.
fn watch() {
    loop {
        std::thread::sleep(RECHECK);
        let local = {
            let mut state = state();
            if state.holds.is_empty() {
                state.watching = false;
                return;
            }
            state.local.upgrade()
        };
        let Some(local) = local else { continue };
        let (keep, lid) = local.keep_awake_settings();
        {
            let mut state = state();
            (state.keep, state.lid) = (keep, lid);
            apply(&mut state);
        }
        local.lid_reconcile();
    }
}

/// The assertion's name the state calls for: none when nothing runs or the setting is off.
fn wanted(state: &State) -> Option<String> {
    if !state.keep {
        return None;
    }
    match state.holds.as_slice() {
        [] => None,
        [(_, what)] => Some(format!("Filo {what}")),
        many => Some(format!("Filo: {} tasks running", many.len())),
    }
}

/// Takes, renames (a new one first, so there's no gap) or lets go of the assertion as the state calls for.
fn apply(state: &mut State) {
    let want = wanted(state);
    if state.held.as_ref().map(|(_, name)| name) != want.as_ref() {
        let new = want.and_then(|name| match power::prevent_idle_sleep(&name) {
            Ok(id) => Some((id, name)),
            Err(code) => {
                eprintln!("awake: couldn't keep the Mac awake for “{name}” (IOKit error {code:#x})");
                None
            }
        });
        if let Some((old, _)) = std::mem::replace(&mut state.held, new) {
            power::release(old);
        }
    }
    // Held with lid mode on: this process counts as a holder for lid.rs; otherwise it doesn't.
    match (state.held.is_some() && state.lid, &state.holder, &state.lid_dir, &state.root) {
        (true, None, Some(dir), Some(root)) => state.holder = Holder::register(dir, root),
        (false, Some(_), _, _) => state.holder = None,
        _ => {}
    }
}

mod power {
    use super::c_void;

    type CFStringRef = *const c_void;

    const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
    const LEVEL_ON: u32 = 255; // kIOPMAssertionLevelOn

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    fn cf_string(text: &str) -> CFStringRef {
        // SAFETY: the bytes are valid UTF-8 for the call; CoreFoundation copies them.
        unsafe { CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0) }
    }

    /// A `PreventUserIdleSystemSleep` assertion named `name`: its id, or IOKit's error.
    pub fn prevent_idle_sleep(name: &str) -> Result<u32, i32> {
        let (kind, name) = (cf_string("PreventUserIdleSystemSleep"), cf_string(name));
        if kind.is_null() || name.is_null() {
            unsafe {
                if !kind.is_null() {
                    CFRelease(kind);
                }
                if !name.is_null() {
                    CFRelease(name);
                }
            }
            return Err(-1);
        }
        let mut id = 0;
        // SAFETY: two valid CFStrings, released after; `id` is written on success.
        let result = unsafe { IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut id) };
        unsafe {
            CFRelease(kind);
            CFRelease(name);
        }
        if result == 0 { Ok(id) } else { Err(result) }
    }

    pub fn release(id: u32) {
        // SAFETY: an id `prevent_idle_sleep` returned, released once.
        unsafe {
            IOPMAssertionRelease(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(keep: bool, holds: &[&str]) -> State {
        State {
            keep,
            holds: holds.iter().enumerate().map(|(at, what)| (at as u64, what.to_string())).collect(),
            ..State::default()
        }
    }

    #[test]
    fn the_assertion_follows_the_work_and_the_setting() {
        assert_eq!(wanted(&with(true, &[])), None);
        assert_eq!(wanted(&with(true, &["coding task: Fix it"])).as_deref(), Some("Filo coding task: Fix it"));
        assert_eq!(wanted(&with(true, &["a", "b"])).as_deref(), Some("Filo: 2 tasks running"));
        assert_eq!(wanted(&with(false, &["a"])), None);
    }

    /// The assertion is real: `pmset -g assertions` lists it while it's held, and not after.
    #[test]
    fn pmset_lists_the_assertion_while_held() {
        let what = format!("test: {}", uuid::Uuid::new_v4());
        let name = format!("Filo {what}");
        let listed = || {
            String::from_utf8_lossy(
                &std::process::Command::new("pmset").args(["-g", "assertions"]).output().unwrap().stdout,
            )
            .contains(&name)
        };
        let mut state = State { keep: true, holds: vec![(1, what)], ..State::default() };
        apply(&mut state);
        assert!(state.held.is_some());
        assert!(listed(), "pmset doesn't list “{name}”");
        state.holds.clear();
        apply(&mut state);
        assert!(state.held.is_none());
        assert!(!listed());
    }
}

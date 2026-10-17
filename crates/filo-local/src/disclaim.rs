// The coding tools don't use Filo's macOS permissions. macOS counts a process's Screen Recording, Accessibility and
// microphone as those of the app responsible for it: by default the app whose process started it, all the way down.
// So the coding tools Filo starts, and every command they run, could capture the screen or drive other apps with
// Filo's grants, without macOS asking. They're started through Filo's own program, `filo --disclaim <program> <args…>`
// (`launch_with`, `command`). That process becomes the program (`posix_spawn` with `POSIX_SPAWN_SETEXEC`, an exec),
// with responsibility disclaimed: the tool and what it starts answer for themselves, and macOS asks for their own
// permissions if they want any. Qt Creator's `disclaim` helper does the same for the programs it runs.
//
// A turn with "Your computer" on keeps Filo's: its helper (computer.rs) works the apps with Filo's Accessibility and
// Screen Recording, which the user granted for it.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Filo's own program, which starts a tool disclaimed; none (tests, the crate's own binary): tools start directly.
static LAUNCHER: OnceLock<PathBuf> = OnceLock::new();

/// The argument that makes Filo's program start the rest disclaimed (`exec`).
pub const ARG: &str = "--disclaim";

/// Starts tools through `program` (Filo's own, as it runs) from now on, in this process.
pub fn launch_with(program: PathBuf) {
    let _ = LAUNCHER.set(program);
}

/// A command that runs `program`: through Filo's program, disclaimed, unless `keep_grants` or there's no launcher.
pub(crate) fn command(program: &Path, keep_grants: bool) -> Command {
    match LAUNCHER.get().filter(|_| !keep_grants) {
        Some(launcher) => {
            let mut command = Command::new(launcher);
            command.arg(ARG).arg(program);
            command
        }
        None => Command::new(program),
    }
}

/// `filo --disclaim <program> <args…>`: becomes `program`, responsible for itself. Returns only when it couldn't.
pub fn exec(args: &[OsString]) -> i32 {
    let Some(program) = args.first() else {
        eprintln!("usage: filo {ARG} <program> [args…]");
        return 2;
    };
    let strings: Result<Vec<CString>, _> = args.iter().map(|arg| CString::new(arg.as_bytes())).collect();
    let Ok(strings) = strings else {
        eprintln!("filo {ARG}: an argument holds a NUL");
        return 2;
    };
    let mut argv: Vec<*mut libc::c_char> = strings.iter().map(|arg| arg.as_ptr() as *mut libc::c_char).collect();
    argv.push(std::ptr::null_mut());
    // SAFETY: the attributes live on this stack until posix_spawnp, which on success never returns (SETEXEC).
    let error = unsafe {
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawnattr_init(&mut attr);
        // As an exec would: signals at their defaults (this program ignores SIGPIPE) and none blocked.
        let mut all: libc::sigset_t = std::mem::zeroed();
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut all);
        libc::sigemptyset(&mut none);
        libc::posix_spawnattr_setsigdefault(&mut attr, &all);
        libc::posix_spawnattr_setsigmask(&mut attr, &none);
        libc::posix_spawnattr_setflags(
            &mut attr,
            (libc::POSIX_SPAWN_SETEXEC | libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK) as libc::c_short,
        );
        disclaim(&mut attr);
        let mut pid: libc::pid_t = 0;
        let error =
            libc::posix_spawnp(&mut pid, strings[0].as_ptr(), std::ptr::null(), &attr, argv.as_ptr(), environ());
        libc::posix_spawnattr_destroy(&mut attr);
        error
    };
    eprintln!(
        "filo {ARG}: couldn't start {}: {}",
        Path::new(program).display(),
        std::io::Error::from_raw_os_error(error)
    );
    127
}

/// Marks `attr` to disclaim responsibility, where this macOS has it (10.14 on; a private call, looked up by name, so a
/// macOS without it starts the program as before).
unsafe fn disclaim(attr: &mut libc::posix_spawnattr_t) {
    type SetDisclaim = unsafe extern "C" fn(*mut libc::posix_spawnattr_t, libc::c_int) -> libc::c_int;
    // SAFETY: the symbol, when there, has this signature (spawn_private.h).
    unsafe {
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_spawnattrs_setdisclaim".as_ptr());
        if !symbol.is_null() {
            let set: SetDisclaim = std::mem::transmute(symbol);
            set(attr, 1);
        }
    }
}

fn environ() -> *const *mut libc::c_char {
    unsafe extern "C" {
        fn _NSGetEnviron() -> *mut *const *mut libc::c_char;
    }
    // SAFETY: the process's environment, as exec passes it on.
    unsafe { *_NSGetEnviron() }
}

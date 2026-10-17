// File locks between Filo's processes (the app, its workers, the scheduler helper, the MCP servers): `flock` on a file
// one of them holds open, retried when a signal interrupts it, and whether a process is still there.
//
// A registry is a folder of such files, one per holder (the app's lid mode holders, the workers): a process `claim`s its
// file and holds it locked while it's there, and the others `look` at each to see whether it's held. A file nobody holds
// (its process killed outright) is removed by the look once it's `STALE_AFTER` old, while the look holds its lock, so a
// process claiming that path at the same moment sees it go and claims the file at the path again.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Duration;

/// A registry file nobody holds is removed once it's this old (one being claimed isn't locked for a moment).
pub const STALE_AFTER: Duration = Duration::from_secs(60);

/// `flock(2)` with `operation` (`LOCK_EX`, `LOCK_SH`, `| LOCK_NB`, `LOCK_UN`), tried again when a signal interrupts it.
pub fn flock(file: &File, operation: libc::c_int) -> io::Result<()> {
    loop {
        // SAFETY: flock on a descriptor this File owns.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Whether a lock failed because another process holds it (rather than for another reason).
pub fn would_block(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EWOULDBLOCK)
}

/// The file's exclusive lock if nobody holds it now; held until the file closes or `unlock`.
pub fn try_lock(file: &File) -> bool {
    flock(file, libc::LOCK_EX | libc::LOCK_NB).is_ok()
}

pub fn unlock(file: &File) {
    let _ = flock(file, libc::LOCK_UN);
}

/// The registry file at `path`, made if it isn't there, locked for this process: none when another process holds it
/// through `tries` tries 10 ms apart (a look holds it for a moment only).
pub fn claim(path: &Path, tries: usize) -> io::Result<Option<File>> {
    for _ in 0..tries {
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        match flock(&file, libc::LOCK_EX | libc::LOCK_NB) {
            Ok(()) => {}
            Err(error) if would_block(&error) => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error),
        }
        // A look may have removed a stale file there between the open and the lock: this lock is then on a file nobody
        // can see, so open the one at the path again.
        let same = file
            .metadata()
            .ok()
            .zip(std::fs::metadata(path).ok())
            .is_some_and(|(open, named)| open.ino() == named.ino() && open.dev() == named.dev());
        if same {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

/// Whether another process holds the registry file at `path` (opened as `file`). One nobody holds is removed while
/// locked here once it's `STALE_AFTER` old.
pub fn look(file: &File, path: &Path) -> bool {
    match flock(file, libc::LOCK_EX | libc::LOCK_NB) {
        Ok(()) => {
            let age = file.metadata().and_then(|meta| meta.modified()).ok().and_then(|at| at.elapsed().ok());
            if age.is_some_and(|age| age > STALE_AFTER) {
                let _ = std::fs::remove_file(path);
            }
            unlock(file);
            false
        }
        Err(error) => would_block(&error),
    }
}

/// Whether process `pid` is still there: signal 0 asks without sending anything (EPERM: there, another user's).
pub fn process_alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0
        && pid <= libc::pid_t::MAX as i64
        && (unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_file_is_claimed_once_and_swept_once_stale() {
        let dir = crate::test_support::temp_dir("lock");
        let path = dir.join("worker.json");
        let mine = claim(&path, 1).unwrap().unwrap();
        // flock is per open file: another open is another holder, as another process would be.
        assert!(claim(&path, 2).unwrap().is_none());
        assert!(look(&File::open(&path).unwrap(), &path));
        drop(mine);
        // Nobody holds it, but it's new: it stays.
        assert!(!look(&File::open(&path).unwrap(), &path));
        assert!(path.exists());
        // Old and nobody's: the look removes it, and a claim makes it again.
        let old = File::options().write(true).open(&path).unwrap();
        old.set_modified(std::time::SystemTime::now() - STALE_AFTER * 2).unwrap();
        assert!(!look(&File::open(&path).unwrap(), &path));
        assert!(!path.exists());
        assert!(claim(&path, 1).unwrap().is_some());
    }
}

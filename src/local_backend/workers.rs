// Conversation turns and coding rounds as processes of their own, so they carry on when Filo quits or restarts.
//
// The app starts each as `filo --worker turn|round …` (its own binary) in a session of its own, its output in
// `local/workers/workers.log`, so quitting, a crash or a restart of the app doesn't reach it. A worker opens the same
// data and does what the app's thread did: the turn (crates/filo-local `turn::run`) or the round
// (local_backend/coding.rs), saving every step to the chat's files and Filo's database as it comes. The app sees that
// through its watcher (local_backend.rs `watch_turns`), so the work shows live whichever process runs it.
//
// Each worker has a file in `local/workers/` (`turn-<turn>.json`, `round-<task>.json`: its pid, its conversation, when it
// started) that it holds locked while it runs; the lock is taken by the app before the worker starts and handed to it,
// so there's no moment a running worker isn't counted. When Filo opens again it adopts the workers still running
// (`adopt`): their turns show as working, their tasks as running (not stopped), and Stop reaches them. Stop is SIGTERM:
// the worker ends its CLI and saves how the work ended, as the app's own thread would have.
//
// Quitting applies "Keep coding after Filo quits" (the Coding page; on by default, crates/filo-local
// `coding::keep_on_quit`): on, the workers carry on; off, quitting stops them as Stop would. The updater's restart
// always keeps them (`restarting`). Results that come while Filo is closed wait for it: a coding round's wake goes to its
// conversation when the app opens again (the app's loop sends wakes, never a worker).

use std::fs::File;
use std::io::{Seek as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use filo_local::engine::Stopper;
use serde::{Deserialize, Serialize};

use super::{Local, Stoppable, now_ms};

/// The folder of worker files, under Filo's local data.
const DIR: &str = "workers";
/// The environment variable that tells a worker which of its descriptors holds its file's lock.
const LOCK_FD: &str = "FILO_WORKER_LOCK";

/// Set by the updater before it quits to relaunch: quitting then keeps the work going whatever the setting.
static RESTARTING: AtomicBool = AtomicBool::new(false);

/// The updater is about to restart Filo: the new one picks the work up.
pub fn restarting() {
    RESTARTING.store(true, Ordering::SeqCst);
}

pub fn is_restarting() -> bool {
    RESTARTING.load(Ordering::SeqCst)
}

/// What a worker does.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// A conversation's turn: the user message `turn` in `session`.
    Turn { session: String, turn: String },
    /// A coding task's next round.
    Round { task: String },
}

impl Job {
    fn file_name(&self) -> String {
        match self {
            Job::Turn { turn, .. } => format!("turn-{turn}.json"),
            Job::Round { task } => format!("round-{task}.json"),
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            Job::Turn { session, turn } => {
                vec!["turn".into(), "--session".into(), session.clone(), "--turn".into(), turn.clone()]
            }
            Job::Round { task } => vec!["round".into(), "--task".into(), task.clone()],
        }
    }
}

/// A worker's file.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// "turn" or "round".
    kind: String,
    /// The turn's or the task's id.
    id: String,
    /// The conversation a turn belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    pid: i32,
    started_at: f64,
}

impl Entry {
    fn job(&self) -> Option<Job> {
        match self.kind.as_str() {
            "turn" => Some(Job::Turn { session: self.session.clone()?, turn: self.id.clone() }),
            "round" => Some(Job::Round { task: self.id.clone() }),
            _ => None,
        }
    }
}

/// A worker running: the app's child, or one a previous run of the app started (adopted).
pub struct Worker {
    pub job: Job,
    pub pid: i32,
    pub started_at: f64,
    end: End,
}

enum End {
    /// The app started it: waiting reaps it.
    Child(Child),
    /// Adopted: its file, whose lock is free once it's gone.
    Lock(File),
}

impl Worker {
    /// SIGTERM asks it to stop its work and save how it ended.
    pub fn stopper(&self) -> Stopper {
        Stopper::for_worker(self.pid)
    }

    /// Blocks until the worker has ended.
    pub fn wait(self) {
        match self.end {
            End::Child(mut child) => {
                let _ = child.wait();
            }
            End::Lock(file) => {
                let _ = lock::flock(&file, libc::LOCK_SH);
            }
        }
    }
}

fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
}

use filo_local::lock;

fn read_entry(path: &Path) -> Option<Entry> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Starts `job` in a worker of its own. When a worker already runs it (another Filo process started it), that one is
/// returned instead, so the same work never runs twice.
pub fn spawn(root: &Path, job: &Job) -> Result<Worker, String> {
    let folder = dir(root);
    std::fs::create_dir_all(&folder).map_err(|error| format!("Couldn't make {}: {error}", folder.display()))?;
    let path = folder.join(job.file_name());
    // Held: a worker runs it (another process's look, `running`, holds it for an instant only).
    let Some(mut file) = lock::claim(&path, 5).map_err(|error| format!("Couldn't lock {}: {error}", path.display()))?
    else {
        let file = File::open(&path).map_err(|error| format!("Couldn't open {}: {error}", path.display()))?;
        let entry = read_entry(&path).unwrap_or_default();
        return Ok(Worker { job: job.clone(), pid: entry.pid, started_at: entry.started_at, end: End::Lock(file) });
    };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join("workers.log"))
        .map_err(|error| format!("Couldn't open the workers' log: {error}"))?;
    let program = std::env::current_exe().map_err(|error| format!("Couldn't find Filo's own program: {error}"))?;
    let fd = file.as_raw_fd();
    let mut command = Command::new(program);
    command
        .arg("--worker")
        .args(job.args())
        .arg("--root")
        .arg(root)
        .env(LOCK_FD, fd.to_string())
        .env(super::LOGIN_PATH, super::login_path())
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log);
    // SAFETY: only async-signal-safe calls between fork and exec. A session of its own, so nothing that happens to the
    // app (quitting, a crash, its terminal closing) reaches it; and the file's descriptor kept across exec, so the lock
    // taken above is the worker's from its first instruction.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|error| format!("Couldn't start a worker: {error}"))?;
    let pid = child.id() as i32;
    let started_at = now_ms() as f64;
    let (kind, id, session) = match job {
        Job::Turn { session, turn } => ("turn", turn.clone(), Some(session.clone())),
        Job::Round { task } => ("round", task.clone(), None),
    };
    let entry = Entry { kind: kind.into(), id, session, pid, started_at };
    let written = serde_json::to_vec(&entry).map_err(|error| error.to_string()).and_then(|bytes| {
        file.set_len(0)
            .and_then(|()| file.rewind())
            .and_then(|()| file.write_all(&bytes))
            .map_err(|error| error.to_string())
    });
    if let Err(error) = written {
        eprintln!("workers: couldn't write {}: {error}", path.display());
    }
    // The app's copy of the descriptor closes here; the worker's keeps the lock.
    drop(file);
    Ok(Worker { job: job.clone(), pid, started_at, end: End::Child(child) })
}

impl Local {
    /// `job` in a worker of its own, which `stop` stops, while work runs in workers (`detaches`). None when it doesn't,
    /// or the worker couldn't start (the log says why): the caller runs the job here.
    pub(super) fn start_worker(&self, job: Job, stop: &Stoppable) -> Option<Worker> {
        if !self.detaches() {
            return None;
        }
        match spawn(&self.root, &job) {
            Ok(worker) => {
                stop.started(worker.stopper());
                Some(worker)
            }
            Err(error) => {
                eprintln!("workers: {error}; running {job:?} in the app");
                None
            }
        }
    }
}

/// The workers running now, whoever started them; files left by those that ended without removing theirs (killed
/// outright) go, once they're a minute old (a file the app is making isn't locked yet for a moment).
pub fn running(root: &Path) -> Vec<Worker> {
    let Ok(files) = std::fs::read_dir(dir(root)) else { return Vec::new() };
    let mut workers = Vec::new();
    for path in files.flatten().map(|file| file.path()).filter(|path| path.extension().is_some_and(|ext| ext == "json"))
    {
        let Ok(file) = File::open(&path) else { continue };
        if !lock::look(&file, &path) {
            continue;
        }
        let Some(entry) = read_entry(&path) else { continue };
        let Some(job) = entry.job() else { continue };
        workers.push(Worker { job, pid: entry.pid, started_at: entry.started_at, end: End::Lock(file) });
    }
    workers
}

// ---------------------------------------------------------------------------------------------------------------
// The worker process

/// A line of the workers' log in one write, so lines from workers writing at once don't interleave.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(format!("{}\n", format_args!($($arg)*)).as_bytes());
    }};
}

/// SIGTERM (Stop, or Filo quitting with the setting off) or SIGINT came.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned()
}

/// `filo --worker turn --session <id> --turn <id> --root <dir>` or `filo --worker round --task <id> --root <dir>`.
pub fn main(args: &[String]) -> i32 {
    let Some(root) = flag(args, "--root").map(PathBuf::from) else {
        eprintln!("usage: filo --worker turn --session <id> --turn <id> --root <dir> | round --task <id> --root <dir>");
        return 2;
    };
    let job = match args.first().map(String::as_str) {
        Some("turn") => {
            flag(args, "--session").zip(flag(args, "--turn")).map(|(session, turn)| Job::Turn { session, turn })
        }
        Some("round") => flag(args, "--task").map(|task| Job::Round { task }),
        _ => None,
    };
    let Some(job) = job else {
        eprintln!("filo --worker: what to do is missing ({args:?})");
        return 2;
    };
    // The lock the app handed over: kept, but not passed on to the CLIs this starts.
    let lock_fd: Option<i32> = std::env::var(LOCK_FD).ok().and_then(|fd| fd.parse().ok());
    if let Some(fd) = lock_fd {
        // SAFETY: fcntl on a descriptor this process inherited and never closes.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    // The app's PATH, read once here; neither variable is the CLIs' business (a `filo` run from a coding task would
    // otherwise take this worker's PATH and lock for its own).
    super::login_path();
    // SAFETY: no other thread runs yet to read the environment.
    unsafe {
        std::env::remove_var(LOCK_FD);
        std::env::remove_var(super::LOGIN_PATH);
    }
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let pid = std::process::id();
    say!("worker {pid}: {job:?}");
    let local = Local::open(root.clone());
    local.in_worker.store(true, Ordering::SeqCst);
    let stop = Stoppable::default();
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !STOP.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
            say!("worker {pid}: asked to stop");
            stop.stop();
        });
    }
    match &job {
        Job::Turn { session, turn } => match local.store_of(session).cloned() {
            Some(store) => {
                let report = local.run_turn_here(&store, session, turn, &stop);
                say!(
                    "worker {pid}: turn ended ({} replies, stopped: {}, error: {:?})",
                    report.replies,
                    report.stopped,
                    report.error
                );
            }
            None => say!("worker {pid}: no conversation {session}"),
        },
        Job::Round { task } => {
            local.round_here(task, &stop);
            say!("worker {pid}: round ended");
        }
    }
    // Its file goes (if it's still this worker's), and the lock with the process.
    let path = dir(&root).join(job.file_name());
    if let Some(fd) = lock_fd {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: fstat on the inherited descriptor into a zeroed struct.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let ours = unsafe { libc::fstat(fd, &mut stat) } == 0
            && std::fs::metadata(&path).is_ok_and(|meta| meta.ino() == stat.st_ino && meta.dev() == stat.st_dev as u64);
        if ours {
            let _ = std::fs::remove_file(&path);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workers_file_names_its_job() {
        let turn =
            Entry { kind: "turn".into(), id: "t-1".into(), session: Some("c-1".into()), pid: 42, started_at: 5. };
        assert_eq!(turn.job(), Some(Job::Turn { session: "c-1".into(), turn: "t-1".into() }));
        let saved: Entry = serde_json::from_str(&serde_json::to_string(&turn).unwrap()).unwrap();
        assert_eq!(saved, turn);
        assert_eq!(
            Entry { kind: "round".into(), id: "k".into(), ..Entry::default() }.job(),
            Some(Job::Round { task: "k".into() })
        );
        assert_eq!(Entry { kind: "turn".into(), id: "t".into(), ..Entry::default() }.job(), None);
        assert_eq!(Job::Round { task: "k".into() }.file_name(), "round-k.json");
        assert_eq!(
            Job::Turn { session: "c".into(), turn: "t".into() }.args(),
            ["turn", "--session", "c", "--turn", "t"]
        );
    }

    /// A file whose lock a process holds counts as a running worker; once the lock is free it's gone.
    #[test]
    fn only_locked_files_are_running_workers() {
        let root = std::env::temp_dir().join(format!("filo-workers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir(&root)).unwrap();
        let entry = Entry { kind: "round".into(), id: "task-1".into(), session: None, pid: 7, started_at: 1. };
        let path = dir(&root).join("round-task-1.json");
        std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
        // A worker killed outright a while ago left its file; one being made isn't locked yet.
        let gone = dir(&root).join("round-gone.json");
        std::fs::write(&gone, b"{}").unwrap();
        File::options()
            .write(true)
            .open(&gone)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        let fresh = dir(&root).join("round-fresh.json");
        std::fs::write(&fresh, b"").unwrap();
        // flock is per open file: a second open of the file is another holder, as another process would be.
        let holder = File::open(&path).unwrap();
        lock::flock(&holder, libc::LOCK_EX).unwrap();
        let found = running(&root);
        assert_eq!(
            found.iter().map(|worker| (worker.job.clone(), worker.pid)).collect::<Vec<_>>(),
            [(Job::Round { task: "task-1".into() }, 7)]
        );
        assert!(!gone.exists() && fresh.exists());
        drop(holder);
        // Its end is the lock coming free.
        found.into_iter().next().unwrap().wait();
        assert!(running(&root).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}

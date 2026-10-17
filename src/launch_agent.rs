// The background helper that fires reminders, routines and check-ins while Filo's window is closed: a per-user
// launchd agent (~/Library/LaunchAgents/im.filo.desktop.scheduler.plist) that keeps `filo --scheduler` running. That's
// the app's own binary without a window: it opens the local data and runs the schedule (local_backend/scheduled.rs),
// leaving the work to the app whenever the app is open. launchd starts it at login, starts it again if it exits, and
// it catches up within seconds after the Mac wakes.
//
// The app installs it once something is scheduled: the first reminder, routine or check-in, however it was made
// (local_backend/scheduled.rs `start`), so a fresh install adds no background item. Once installed, the app refreshes
// it when it opens, so it follows the binary when it changed (a rebuild or an update) and always runs the app's own
// code (`ensure_installed`). Only Filo.app installs it: macOS shows banners only for a signed
// app bundle, so the bare binary (`./target/debug/filo`) leaves the installed helper alone. Not with a temporary `FILO_CONFIG_DIR` (checks and tests),
// unless `FILO_LAUNCH_AGENT=1`, and then under a label of its own so it never replaces the real one;
// `FILO_NO_LAUNCH_AGENT=1` never installs it.
//
//   filo --scheduler             the helper itself (what launchd runs)
//   filo --scheduler install     install or refresh it now
//   filo --scheduler uninstall   stop it and remove it
//   filo --scheduler status      whether it's installed and running

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;

const LABEL: &str = "im.filo.desktop.scheduler";

/// The agent's label: its own for a non-default data folder.
fn label() -> String {
    label_for(LABEL)
}

/// An agent's label from its base (this helper's, or local_backend/lid.rs's backstop): its own for a non-default data
/// folder, so a check's agent never replaces the real one.
pub(crate) fn label_for(base: &str) -> String {
    match std::env::var("FILO_CONFIG_DIR").ok().filter(|dir| !dir.is_empty()) {
        Some(dir) => format!("{base}.{}", &hex::encode(sha2::Sha256::digest(dir.as_bytes()))[..8]),
        None => base.to_string(),
    }
}

fn plist_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents").join(format!("{}.plist", label()))
}

fn log_path() -> PathBuf {
    crate::config::config_dir().join("local").join("scheduler.log")
}

pub(crate) fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Whether this run of the app should install the helper.
pub fn wanted() -> bool {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0");
    !set("FILO_NO_LAUNCH_AGENT") && (!set("FILO_CONFIG_DIR") || set("FILO_LAUNCH_AGENT"))
}

pub(crate) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The binary's size and modification time: a new build changes the plist, so the helper is restarted with it.
fn stamp(exe: &Path) -> String {
    let meta = std::fs::metadata(exe).ok();
    let modified = meta
        .as_ref()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
    format!("{}-{}", meta.map_or(0, |meta| meta.len()), modified.map_or(0, |time| time.as_secs()))
}

/// The agent's plist: run `exe --scheduler` at login and keep it running.
pub fn plist(label: &str, exe: &Path, env: &[(String, String)], log: &Path) -> String {
    let env: String = env
        .iter()
        .map(|(key, value)| format!("\t\t<key>{}</key>\n\t\t<string>{}</string>\n", xml_escape(key), xml_escape(value)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>--scheduler</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
{env}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>30</integer>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&exe.display().to_string()),
        log = xml_escape(&log.display().to_string()),
    )
}

pub(crate) fn launchctl(args: &[&str]) -> Result<(), String> {
    let output = Command::new("/bin/launchctl").args(args).output().map_err(|error| format!("launchctl: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!("launchctl {}: {said} ({})", args.join(" "), output.status))
}

fn loaded() -> bool {
    Command::new("/bin/launchctl")
        .args(["print", &format!("{}/{}", domain(), label())])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Installs the helper, or brings it up to date with this binary; `Ok(true)` when it (re)started it.
pub fn ensure_installed() -> Result<bool, String> {
    if !crate::recorder::in_app_bundle() {
        return Err(
            "Only Filo.app installs the background scheduler (scripts/app.sh), so its reminders show as Filo's banners"
                .into(),
        );
    }
    let exe = std::env::current_exe().map_err(|error| format!("Where Filo is: {error}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let mut env = vec![("FILO_SCHEDULER_STAMP".to_string(), stamp(&exe))];
    for name in ["SHELL", "FILO_CONFIG_DIR"] {
        if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
            env.push((name.to_string(), value));
        }
    }
    let log = log_path();
    let content = plist(&label(), &exe, &env, &log);
    let path = plist_path();
    let was_loaded = loaded();
    if was_loaded && std::fs::read_to_string(&path).is_ok_and(|current| current == content) {
        return Ok(false);
    }
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    crate::config::write_atomic(&path, content.as_bytes())
        .map_err(|error| format!("Couldn't write {}: {error}", path.display()))?;
    if was_loaded {
        let _ = launchctl(&["bootout", &format!("{}/{}", domain(), label())]);
    }
    // Booting out finishes in the background: bootstrap can fail for a moment after it.
    let mut last = Ok(());
    for _ in 0..10 {
        last = launchctl(&["bootstrap", &domain(), &path.display().to_string()]);
        if last.is_ok() || loaded() {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    last.map(|_| true)
}

pub fn uninstall() -> Result<(), String> {
    if loaded() {
        launchctl(&["bootout", &format!("{}/{}", domain(), label())])?;
    }
    match std::fs::remove_file(plist_path()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("Couldn't remove {}: {error}", plist_path().display()))
        }
        _ => Ok(()),
    }
}

/// Set once the app has installed or refreshed the helper: once per run is enough.
static ENSURED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From the app, once something is scheduled: off the UI thread, since launchctl takes a moment. Once per run.
pub fn install_in_background() {
    if !wanted() || !crate::recorder::in_app_bundle() || ENSURED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| match ensure_installed() {
        Ok(true) => eprintln!("filo: the background scheduler is installed ({})", label()),
        Ok(false) => {}
        Err(error) => eprintln!(
            "filo: the background scheduler couldn't be installed, so reminders only fire while Filo is open: {error}"
        ),
    });
}

/// From the app as it opens: brings an installed helper up to date with this binary; installs nothing.
pub fn refresh_in_background() {
    if plist_path().exists() {
        install_in_background();
    }
}

/// `filo --scheduler [install|uninstall|status]`. Returns the exit code; the helper itself never returns.
pub fn main(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            let local = crate::local_backend::Local::open(crate::config::config_dir().join("local"));
            eprintln!(
                "filo: background scheduler running for {} (pid {})",
                crate::config::config_dir().display(),
                std::process::id()
            );
            crate::local_backend::start_schedule(local, true);
            loop {
                std::thread::park();
            }
        }
        Some("install") => match ensure_installed() {
            Ok(started) => {
                println!(
                    "{} {} ({})",
                    label(),
                    if started { "installed and started" } else { "already up to date" },
                    plist_path().display()
                );
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        Some("uninstall") => match uninstall() {
            Ok(()) => {
                println!("{} removed", label());
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        },
        Some("status") => {
            let installed = plist_path().exists();
            println!(
                "{}: {}, {}",
                label(),
                if installed { "installed" } else { "not installed" },
                if loaded() { "running" } else { "not running" }
            );
            println!("plist: {}\nlog: {}", plist_path().display(), log_path().display());
            0
        }
        Some(other) => {
            eprintln!("usage: filo --scheduler [install|uninstall|status] (not {other:?})");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_the_helper_and_keeps_it_alive() {
        let env = vec![
            ("SHELL".to_string(), "/bin/zsh".to_string()),
            ("FILO_CONFIG_DIR".to_string(), "/tmp/a&b".to_string()),
        ];
        let plist = plist(
            LABEL,
            Path::new("/Applications/Filo.app/Contents/MacOS/filo"),
            &env,
            Path::new("/tmp/scheduler.log"),
        );
        assert!(plist.contains("<string>im.filo.desktop.scheduler</string>"));
        assert!(
            plist.contains(
                "<string>/Applications/Filo.app/Contents/MacOS/filo</string>\n\t\t<string>--scheduler</string>"
            )
        );
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>") && plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<string>/tmp/a&amp;b</string>"));
        // plutil checks it's a property list macOS reads.
        let path = std::env::temp_dir().join(format!("filo-plist-{}.plist", uuid::Uuid::new_v4()));
        std::fs::write(&path, &plist).unwrap();
        let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
        assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        std::fs::remove_file(path).ok();
    }
}

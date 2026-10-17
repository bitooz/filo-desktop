// Test windows never ask macOS for anything. Coding tasks open smoke runs and fixtures by the dozen (FILO_SMOKE,
// FILO_OPEN, FILO_FIXTURE_SHOTS and the other drive and shot switches, or a FILO_CONFIG_DIR of their own), and every
// capture or permission ask from one of them is a dialog on the user's screen: "Filo would like to record this
// computer's screen" came up some 200 times one night (2026-10-13), once per `screencapture` of a test window. So in a
// test run nothing captures the screen, and nothing asks for Screen Recording, Accessibility, the microphone,
// notifications or an administrator's password: each place that would calls `refuse` first and does without, and
// the test below fails when such a call is added anywhere else or without it. Test windows' own screenshots are made
// in the app (window_shot.rs).

use std::path::PathBuf;
use std::sync::OnceLock;

/// The switches smoke runs, fixtures and their drives start with.
const SWITCHES: [&str; 12] = [
    "FILO_SMOKE",
    "FILO_OPEN",
    "FILO_FIXTURE_SHOTS",
    "FILO_FIXTURE_DRIVE",
    "FILO_FIXTURE_FRAMES",
    "FILO_FIXTURE_PLAN",
    "FILO_DRIVE_SHOTS",
    "FILO_SHOT_AFTER",
    "FILO_PET_DRIVE",
    "FILO_QUIT_DRIVE",
    "FILO_WELCOME_DRIVE",
    "FILO_COMPARE_DRIVE",
];

/// Whether this process is a smoke run, a fixture or another check: one of `SWITCHES` is set, or FILO_CONFIG_DIR is a
/// folder other than the real ~/.config/filo.
pub fn active() -> bool {
    static ACTIVE: OnceLock<bool> = OnceLock::new();
    *ACTIVE.get_or_init(|| {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        SWITCHES.iter().any(|name| set(name)) || own_config()
    })
}

/// Whether FILO_CONFIG_DIR names a data folder other than the user's ~/.config/filo.
pub fn own_config() -> bool {
    std::env::var_os("FILO_CONFIG_DIR").filter(|dir| !dir.is_empty()).is_some_and(|dir| !is_real_config(dir.into()))
}

fn is_real_config(dir: PathBuf) -> bool {
    let real = dirs::home_dir().unwrap_or_default().join(".config/filo");
    std::fs::canonicalize(&dir).unwrap_or(dir) == std::fs::canonicalize(&real).unwrap_or(real)
}

/// A test server or setting a check points Filo at (`FILO_WORKER_ORIGIN`, `FILO_RESULTS_URL`, `FILO_ARTIFACTS_URL`,
/// `FILO_REPORT_URL`, `FILO_UPDATE_FEED`): read in `dev` builds only, so nothing in the packaged app's environment can
/// send shares, their tokens, problem reports, voice or updates elsewhere.
pub fn dev_override(name: &str) -> Option<String> {
    if !cfg!(feature = "dev") {
        return None;
    }
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// `FILO_WINDOW=<width>x<height>` (`dev` builds: the app's window, the fixtures'), else `default`. A test window's
/// clicks and keys find it by its width.
pub fn window_size(default: (f32, f32)) -> (f32, f32) {
    std::env::var("FILO_WINDOW")
        .ok()
        .filter(|_| cfg!(feature = "dev"))
        .and_then(|value| {
            value
                .split_once('x')
                .and_then(|(width, height)| Some((width.trim().parse().ok()?, height.trim().parse().ok()?)))
        })
        .unwrap_or(default)
}

/// True in a test run, after logging that `what` is left out: the caller then goes without it, as if macOS had said no.
pub fn refuse(what: &str) -> bool {
    let refused = active();
    if refused {
        eprintln!(
            "test run: no {what} (test windows never capture the screen or ask macOS for a permission; test_run.rs)"
        );
    }
    refused
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// What captures the screen or asks macOS for a permission (with a prompt, or by starting what brings one up).
    const ASKS: [&str; 19] = [
        "CGRequestScreenCaptureAccess",
        "\"screencapture\"",
        "/usr/sbin/screencapture",
        "CGWindowListCreateImage",
        "CGDisplayCreateImage",
        "CGDisplayStream",
        "SCScreenshotManager",
        "SCShareableContent",
        "AXIsProcessTrustedWithOptions",
        "CGRequestPostEventAccess",
        "CGRequestListenEventAccess",
        "IOHIDRequestAccess",
        "CGEventTapCreate",
        "requestAccessForMediaType",
        "requestRecordPermission",
        "prepareToRecord",
        "startAndReturnError",
        "requestAuthorizationWithOptions",
        "osascript",
    ];

    /// The only files that may make those calls, each from a function that starts with `refuse`.
    const GUARDED: [&str; 5] = [
        "src/screenshot.rs",
        "src/computer_use.rs",
        "src/recorder.rs",
        "src/notifications.rs",
        "src/local_backend/lid.rs",
    ];

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("source folder").flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                out.push(path);
            }
        }
    }

    /// What's wrong in the file `name` (from the repo's root) with `text`.
    fn problems(name: &str, text: &str) -> Vec<String> {
        let function = regex::Regex::new(
            r#"^\s*(pub(\([^)]*\))?\s+)?(const\s+)?(async\s+)?(unsafe\s+)?(extern\s+"[^"]*"\s+)?fn\s"#,
        )
        .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut problems = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            // Comments, and the declarations in `extern` blocks.
            if code.starts_with("//") || (code.starts_with("fn ") && code.ends_with(';')) {
                continue;
            }
            let Some(ask) = ASKS.iter().find(|ask| line.contains(*ask)) else { continue };
            if !GUARDED.contains(&name) {
                problems.push(format!("{name}:{}: {ask} outside the guarded files (go through one of them, or add a file there with a refuse check)", index + 1));
                continue;
            }
            let start = (0..=index).rev().find(|line| function.is_match(lines[*line])).unwrap_or(0);
            if !lines[start..index].iter().any(|line| line.contains("test_run::refuse(")) {
                problems.push(format!(
                    "{name}:{}: {ask} in a function that doesn't start with crate::test_run::refuse",
                    index + 1
                ));
            }
        }
        problems
    }

    #[test]
    fn screen_capture_and_permission_asks_go_through_refuse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        rust_files(&root.join("crates/filo-local/src"), &mut files);
        let mut found = Vec::new();
        for file in files {
            let name = file.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if name != "src/test_run.rs" {
                found.extend(problems(&name, &std::fs::read_to_string(&file).unwrap()));
            }
        }
        assert!(
            found.is_empty(),
            "test windows must never capture the screen or ask for a permission:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn an_ask_without_refuse_or_outside_the_guarded_files_is_caught() {
        let unguarded =
            "pub fn request() {\n    center.requestAuthorizationWithOptions_completionHandler(options, &handler);\n}\n";
        assert_eq!(problems("src/notifications.rs", unguarded).len(), 1);
        let guarded = "pub fn request() {\n    if crate::test_run::refuse(\"notifications prompt\") {\n        return;\n    }\n    center.requestAuthorizationWithOptions_completionHandler(options, &handler);\n}\n";
        assert!(problems("src/notifications.rs", guarded).is_empty());
        // A guard in the function before doesn't count.
        let earlier = format!(
            "{guarded}\nfn again() {{\n    center.requestAuthorizationWithOptions_completionHandler(options, &handler);\n}}\n"
        );
        assert_eq!(problems("src/notifications.rs", &earlier).len(), 1);
        let shot =
            "fn shot(path: &str) {\n    let _ = std::process::Command::new(\"screencapture\").arg(path).status();\n}\n";
        assert_eq!(problems("src/screens/some_fixture.rs", shot).len(), 1);
        assert!(
            problems("src/screens/some_fixture.rs", "// screencapture used to take these (\"screencapture\")\n")
                .is_empty()
        );
    }
}

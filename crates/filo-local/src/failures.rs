// The recent failures on this Mac, for a problem report (the app's report.rs): a chat turn or a coding round that
// ended with an error, with the CLI's last lines on stderr, which the chat doesn't keep. One JSON line each in
// `failures.jsonl` beside the chats, the latest `KEEP` only. No chat text: the error as the chat shows it, and stderr.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::Engine;

const FILE: &str = "failures.jsonl";
/// How many failures the file keeps.
const KEEP: usize = 30;
/// The stderr lines kept with each.
const STDERR_LINES: usize = 40;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    /// Milliseconds since the epoch.
    pub at: u64,
    /// "turn" (a chat's reply) or "round" (a coding task's).
    pub kind: String,
    /// The turn's or the coding task's id.
    pub id: String,
    pub engine: Option<Engine>,
    #[serde(default)]
    pub model: Option<String>,
    pub error: String,
    #[serde(default)]
    pub stderr: Vec<String>,
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Adds `failure` to the file in `dir`, dropping the oldest past `KEEP`.
pub fn record(dir: &Path, mut failure: Failure) {
    let excess = failure.stderr.len().saturating_sub(STDERR_LINES);
    failure.stderr.drain(..excess);
    let Ok(line) = serde_json::to_string(&failure) else { return };
    let path = path(dir);
    let mut lines: Vec<String> =
        std::fs::read_to_string(&path).map(|text| text.lines().map(str::to_string).collect()).unwrap_or_default();
    if lines.len() >= KEEP {
        lines.push(line);
        let keep = lines.split_off(lines.len() - KEEP);
        let _ = crate::files::write_atomic(&path, (keep.join("\n") + "\n").as_bytes());
        return;
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

/// The failures in `dir`, latest first.
pub fn recent(dir: &Path) -> Vec<Failure> {
    let text = std::fs::read_to_string(path(dir)).unwrap_or_default();
    let mut failures: Vec<Failure> = text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect();
    failures.reverse();
    failures
}

/// The failure of turn or task `id`, the latest when it failed more than once.
pub fn find(dir: &Path, id: &str) -> Option<Failure> {
    recent(dir).into_iter().find(|failure| failure.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(id: &str, stderr: usize) -> Failure {
        Failure {
            at: 1,
            kind: "turn".into(),
            id: id.into(),
            engine: Some(Engine::Claude),
            model: None,
            error: format!("failed {id}"),
            stderr: (0..stderr).map(|n| format!("line {n}")).collect(),
        }
    }

    #[test]
    fn keeps_the_latest_with_their_stderr() {
        let dir = crate::test_support::temp_dir("failures");
        assert!(recent(&dir).is_empty());
        record(&dir, failure("a", 50));
        let found = find(&dir, "a").unwrap();
        assert_eq!(found.stderr.len(), STDERR_LINES);
        assert_eq!(found.stderr.last().map(String::as_str), Some("line 49"));
        for n in 0..KEEP + 5 {
            record(&dir, failure(&format!("t{n}"), 1));
        }
        let all = recent(&dir);
        assert_eq!(all.len(), KEEP);
        assert_eq!(all[0].id, format!("t{}", KEEP + 4));
        assert!(find(&dir, "a").is_none());
    }
}

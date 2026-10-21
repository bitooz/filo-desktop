// Which of pi's sign-ins work, so the model pickers grey out the models of a provider whose sign-in doesn't (pi's
// `get_available_models` lists every provider with credentials, a sign-in that can't be renewed included).
//
// pi says itself: `pi auth check --provider <id> --json` (`ready`, `not_ready` or `invalid`), which renews an OAuth
// sign-in about to run out the way any pi run does, under pi's own lock, and finds one whose renewal is refused. Filo
// never writes pi's `auth.json`; it reads it only to know when a provider's credential changed. What a check can't tell
// (an API key that ran out, a model the sign-in doesn't reach, like a non-Codex model with a ChatGPT account) comes from
// a reply that failed on it (`learn`).
//
// Each finding is a mark in Filo's database, kept with a fingerprint of the provider's credential: a provider's while
// its entry in `auth.json` is the same, a model's while the provider's sign-in is the same kind (its tokens renewed or
// not). A new /login changes the entry, and its marks stop counting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::params;
use serde_json::Value;

use crate::db::Db;
use crate::engine::{Engine, ModelOption};

/// The tables, made beside the migrations (db.rs), so a Filo that doesn't know them still opens the file.
/// `pi_logins`: a provider (`openai-codex`) or a model (`openai/gpt-5.5`) that doesn't answer, why, the credential's
/// fingerprint it holds for, and whether a check found it (replaced by the next check) or a reply did.
/// `pi_login_looks`: when `auth.json` had last changed as of the last check.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pi_logins (
    key     TEXT PRIMARY KEY NOT NULL,
    reason  TEXT NOT NULL,
    stamp   TEXT NOT NULL,
    checked INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pi_login_looks (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    auth_at INTEGER
);
";

/// The picker's word for a provider whose sign-in pi can't use (ran out, revoked).
pub const SIGN_IN_AGAIN: &str = "Sign in again";
/// For a provider pi has no credentials for.
pub const NOT_SIGNED_IN: &str = "Not signed in";
/// For a model the provider's sign-in doesn't reach.
pub const NOT_WITH_SIGN_IN: &str = "Not with this sign-in";

/// How long a `pi auth check` gets: longer than pi's own 15 seconds for renewing a sign-in, so a renewal under way
/// is never cut off (it may already have replaced the old token).
const CHECK_FOR: Duration = Duration::from_secs(30);

pub fn ensure_tables(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

/// pi's folder: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn agent_dir() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".pi/agent"))
}

/// When pi's `auth.json` last changed (ms), None without one: a /login, a logout or a renewal.
pub fn auth_changed_at() -> Option<i64> {
    let modified = std::fs::metadata(agent_dir().join("auth.json")).ok()?.modified().ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// pi's credentials as `auth.json` has them, read only for their fingerprints.
pub struct Auth(Value);

impl Auth {
    pub fn read() -> Auth {
        Auth::at(&agent_dir().join("auth.json"))
    }

    pub fn at(file: &Path) -> Auth {
        Auth(std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null))
    }

    /// The fingerprint a provider's mark holds for: its whole entry.
    fn whole(&self, provider: &str) -> String {
        crate::engine::fingerprint(&self.0[provider].to_string())
    }

    /// The fingerprint a model's mark holds for: the entry less what a renewal changes.
    fn kind(&self, provider: &str) -> String {
        let mut entry = self.0[provider].clone();
        if let Some(entry) = entry.as_object_mut() {
            for renewed in ["access", "refresh", "expires"] {
                entry.remove(renewed);
            }
        }
        crate::engine::fingerprint(&entry.to_string())
    }

    fn stamp(&self, key: &str) -> String {
        match key.split_once('/') {
            Some((provider, _)) => self.kind(provider),
            None => self.whole(key),
        }
    }
}

/// The provider of a pi model id (`openrouter/google/gemini-3.5-flash`: `openrouter`).
pub fn provider_of(model: &str) -> &str {
    model.split_once('/').map_or(model, |(provider, _)| provider)
}

/// The marks that hold for `auth` as it is, by provider or model; those that don't are forgotten.
pub fn marks(db: &Db, auth: &Auth) -> HashMap<String, String> {
    let rows: Vec<(String, String, String)> = db
        .with(|conn| {
            let mut statement = conn.prepare("SELECT key, reason, stamp FROM pi_logins")?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    let (holding, stale): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(key, _, stamp)| auth.stamp(key) == *stamp);
    if !stale.is_empty() {
        let _ = db.write(|tx| {
            for (key, _, _) in &stale {
                tx.execute("DELETE FROM pi_logins WHERE key = ?1", params![key])?;
            }
            Ok(())
        });
    }
    holding.into_iter().map(|(key, reason, _)| (key, reason)).collect()
}

fn mark(tx: &rusqlite::Transaction, auth: &Auth, key: &str, reason: &str, checked: bool) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO pi_logins (key, reason, stamp, checked) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET reason = excluded.reason, stamp = excluded.stamp, checked = excluded.checked",
        params![key, reason, auth.stamp(key), checked as i64],
    )
}

/// What `pi auth check` found (`check`): the providers that don't work, with why, replace the last check's, and
/// `auth_at` is when `auth.json` had changed as of it. What replies found stays.
pub fn checked(db: &Db, auth: &Auth, dead: &[(String, String)], auth_at: Option<i64>) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("DELETE FROM pi_logins WHERE checked = 1", [])?;
        for (provider, reason) in dead {
            mark(tx, auth, provider, reason, true)?;
        }
        tx.execute(
            "INSERT INTO pi_login_looks (id, auth_at) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET auth_at = excluded.auth_at",
            params![auth_at],
        )?;
        Ok(())
    })
}

/// When `auth.json` had changed as of the last check (`checked`); None before one.
pub fn checked_auth_at(db: &Db) -> Option<Option<i64>> {
    use rusqlite::OptionalExtension as _;
    db.with(|conn| conn.query_row("SELECT auth_at FROM pi_login_looks WHERE id = 1", [], |row| row.get(0)).optional())
        .ok()
        .flatten()
}

/// Whether the last check found a provider that doesn't work: worth asking again now and then, as a check made while
/// the Mac was offline finds a renewal refused too.
pub fn has_checked_marks(db: &Db) -> bool {
    db.with(|conn| conn.query_row("SELECT EXISTS (SELECT 1 FROM pi_logins WHERE checked = 1)", [], |row| row.get(0)))
        .unwrap_or(false)
}

/// A pi reply on `model` failed with `error`: the provider or model it says doesn't work is marked. True when one is.
pub fn learn(db: &Db, auth: &Auth, model: Option<&str>, error: &str) -> bool {
    let Some((key, reason)) = refusal(model, error) else { return false };
    db.write(|tx| mark(tx, auth, &key, reason, false)).is_ok()
}

/// The provider or model a failed reply says doesn't work, and the picker's word for it: a sign-in pi couldn't renew
/// (it names the provider), a model the sign-in doesn't reach, a key that ran out or was refused.
fn refusal(model: Option<&str>, error: &str) -> Option<(String, &'static str)> {
    let lower = error.to_lowercase();
    if let Some(at) = lower.find("oauth refresh failed for ") {
        let named: String = error[at + "oauth refresh failed for ".len()..]
            .chars()
            .take_while(|char| char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.'))
            .collect();
        if !named.is_empty() {
            return Some((named, SIGN_IN_AGAIN));
        }
    }
    let model = model.filter(|model| model.contains('/'))?;
    if lower.contains("is not supported when using") || lower.contains("model is not supported") {
        return Some((model.to_string(), NOT_WITH_SIGN_IN));
    }
    Engine::signed_out(error).then(|| (provider_of(model).to_string(), SIGN_IN_AGAIN))
}

/// What to do about a failed pi reply on `model` whose sign-in or model is the reason: pick another model, or sign in
/// to that provider again. None for any other failure.
pub fn hint(model: Option<&str>, error: &str) -> Option<String> {
    let (key, reason) = refusal(model, error).or_else(|| {
        // A sign-in pi couldn't use, on its default model: which provider isn't known.
        Engine::signed_out(error).then(|| (String::new(), SIGN_IN_AGAIN))
    })?;
    Some(match (reason, key.as_str()) {
        (NOT_WITH_SIGN_IN, model) => format!(
            "pi's sign-in to {} doesn't reach {}. Pick another model in the model picker.",
            provider_of(model),
            model.split_once('/').map_or(model, |(_, id)| id)
        ),
        (_, "") => "Pick a model of a provider you're signed in to in the model picker (the ones that don't work \
                    are greyed out), or run `pi` in Terminal and type /login to sign in again."
            .into(),
        (_, provider) => format!(
            "pi's sign-in to {provider} doesn't work anymore. Pick a model of another provider in the model picker \
             (the ones that don't work are greyed out), or run `pi` in Terminal and type /login to sign in to \
             {provider} again."
        ),
    })
}

/// `rows` with pi's models marked unavailable as `marks` say (a model's own mark, else its provider's); the rest left.
pub fn apply(rows: &mut [ModelOption], marks: &HashMap<String, String>) {
    for row in rows.iter_mut().filter(|row| row.engine == Engine::Pi) {
        row.unavailable = marks.get(&row.id).or_else(|| marks.get(provider_of(&row.id))).cloned();
    }
}

/// Asks pi whether each of `providers`' sign-ins works (`pi auth check`, all at once): the ones that don't, with the
/// picker's word for why. A provider pi couldn't answer for isn't among them.
pub fn check(program: &Path, path_env: Option<&str>, providers: &[String]) -> Vec<(String, String)> {
    std::thread::scope(|scope| {
        let asks: Vec<_> = providers
            .iter()
            .map(|provider| (provider, scope.spawn(move || check_one(program, path_env, provider))))
            .collect();
        asks.into_iter().filter_map(|(provider, ask)| Some((provider.clone(), ask.join().ok()??.to_string()))).collect()
    })
}

/// One provider's `pi auth check --json`: None when it works or pi couldn't say, else the picker's word.
fn check_one(program: &Path, path_env: Option<&str>, provider: &str) -> Option<&'static str> {
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut command = crate::disclaim::command(program, false);
    command
        .args(["auth", "check", "--provider", provider, "--json"])
        .env("PI_OFFLINE", "1")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + CHECK_FOR;
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    verdict(&out)
}

/// `pi auth check --json`'s answer as the picker's word: none for `ready`, or for an answer it can't read.
fn verdict(out: &str) -> Option<&'static str> {
    let answer: Value = out.lines().rev().find_map(|line| serde_json::from_str(line.trim()).ok())?;
    match (answer["status"].as_str()?, answer["reason"].as_str().unwrap_or_default()) {
        ("ready", _) => None,
        ("not_ready", "credentials_not_configured") => Some(NOT_SIGNED_IN),
        ("not_ready", "provider_not_found") => None,
        _ => Some(SIGN_IN_AGAIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> ModelOption {
        ModelOption {
            engine: Engine::Pi,
            id: id.into(),
            label: id.into(),
            default: false,
            aliases: Vec::new(),
            unavailable: None,
        }
    }

    #[test]
    fn pi_auth_check_answers_read_as_the_pickers_words() {
        assert_eq!(verdict(r#"{"status":"ready","provider":"openai","authType":"oauth"}"#), None);
        assert_eq!(
            verdict(r#"{"status":"invalid","provider":"openai-codex","reason":"invalid_state"}"#),
            Some(SIGN_IN_AGAIN)
        );
        assert_eq!(
            verdict(r#"{"status":"not_ready","provider":"anthropic","reason":"credentials_not_configured"}"#),
            Some(NOT_SIGNED_IN)
        );
        assert_eq!(verdict(r#"{"status":"not_ready","provider":"x","reason":"provider_not_found"}"#), None);
        assert_eq!(verdict("Error: something\n"), None);
    }

    /// Tonight's failures as pi worded them, and what each marks.
    #[test]
    fn a_failed_reply_names_what_doesnt_work() {
        let refresh = "pi: OAuth refresh failed for openai-codex: OpenAI Codex token refresh failed (401): {\n  \"error\": { \"code\": \"invalid_refresh_token\" } }";
        assert_eq!(refusal(Some("openai-codex/gpt-6.1-sol"), refresh), Some(("openai-codex".into(), SIGN_IN_AGAIN)));
        // On pi's default model too: the error names the provider.
        assert_eq!(refusal(None, refresh), Some(("openai-codex".into(), SIGN_IN_AGAIN)));
        let chatgpt = "pi: OpenAI API error (400): 400 {\"detail\":\"The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account.\"}";
        assert_eq!(refusal(Some("openai/gpt-5.5"), chatgpt), Some(("openai/gpt-5.5".into(), NOT_WITH_SIGN_IN)));
        let expired = "pi: 401: {\"message\":\"API key expired.\",\"code\":401}";
        assert_eq!(refusal(Some("openrouter/openai/gpt-6.1-sol"), expired), Some(("openrouter".into(), SIGN_IN_AGAIN)));
        assert_eq!(refusal(None, expired), None);
        assert_eq!(refusal(Some("openai/gpt-5.5"), "pi: Request timed out."), None);
    }

    #[test]
    fn the_hint_points_at_a_model_that_works() {
        let refresh = "pi: OAuth refresh failed for openai-codex: OpenAI Codex token refresh failed (401)";
        let hint_text = hint(Some("openai-codex/gpt-6.1-sol"), refresh).unwrap();
        assert!(hint_text.contains("sign-in to openai-codex doesn't work anymore"), "{hint_text}");
        assert!(hint_text.contains("Pick a model of another provider in the model picker"), "{hint_text}");
        assert!(hint_text.contains("/login"), "{hint_text}");
        let chatgpt = "400 The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account.";
        assert_eq!(
            hint(Some("openai/gpt-5.5"), chatgpt).unwrap(),
            "pi's sign-in to openai doesn't reach gpt-5.5. Pick another model in the model picker."
        );
        assert!(
            hint(None, "401: API key expired.").unwrap().starts_with("Pick a model of a provider you're signed in to")
        );
        assert_eq!(hint(Some("openai/gpt-5.5"), "Request timed out."), None);
    }

    /// A mark holds while the provider's credential is the one it was made with: a /login replaces it, and a renewal
    /// keeps a model's mark (the sign-in is the same kind) but not a provider's.
    #[test]
    fn marks_hold_until_the_sign_in_changes() {
        let db = Db::in_memory().unwrap();
        let dead = auth("dead", "first");
        assert!(learn(&db, &dead, Some("openai-codex/gpt-6.1-sol"), "OAuth refresh failed for openai-codex: 401"));
        assert!(learn(
            &db,
            &dead,
            Some("openai/gpt-5.5"),
            "The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account."
        ));
        assert!(learn(&db, &dead, Some("openrouter/x/y"), "401: API key expired."));
        assert!(!learn(&db, &dead, Some("openrouter/x/y"), "Request timed out."));
        let mut rows = vec![
            row("openai-codex/gpt-6.1-sol"),
            row("openai/gpt-5.5"),
            row("openai/gpt-6.1-sol"),
            row("openrouter/x/y"),
        ];
        apply(&mut rows, &marks(&db, &dead));
        let shown: Vec<Option<&str>> = rows.iter().map(|row| row.unavailable.as_deref()).collect();
        assert_eq!(shown, [Some(SIGN_IN_AGAIN), Some(NOT_WITH_SIGN_IN), None, Some(SIGN_IN_AGAIN)]);

        // openai's tokens renewed: its model's mark holds. A /login to openai-codex: its mark goes.
        let later = auth("signed-in-again", "renewed");
        let marked = marks(&db, &later);
        assert_eq!(marked.get("openai/gpt-5.5").map(String::as_str), Some(NOT_WITH_SIGN_IN));
        assert!(!marked.contains_key("openai-codex"));
        assert_eq!(marked.get("openrouter").map(String::as_str), Some(SIGN_IN_AGAIN));
        apply(&mut rows, &marked);
        assert_eq!(rows[0].unavailable, None);
    }

    /// A check replaces the last check's marks, and leaves what replies found.
    #[test]
    fn a_check_replaces_the_last_checks_marks() {
        let db = Db::in_memory().unwrap();
        let now = auth("dead", "first");
        assert_eq!(checked_auth_at(&db), None);
        checked(&db, &now, &[("openai-codex".into(), SIGN_IN_AGAIN.into())], Some(5)).unwrap();
        assert!(learn(&db, &now, Some("openrouter/x/y"), "401: API key expired."));
        assert!(has_checked_marks(&db));
        assert_eq!(checked_auth_at(&db), Some(Some(5)));
        checked(&db, &now, &[], Some(6)).unwrap();
        let marked = marks(&db, &now);
        assert!(!marked.contains_key("openai-codex") && marked.contains_key("openrouter"), "{marked:?}");
        assert!(!has_checked_marks(&db));
        assert_eq!(checked_auth_at(&db), Some(Some(6)));
    }

    /// pi's auth.json: a sign-in under `openai-codex` (`codex`: its refresh token), a ChatGPT one under `openai`
    /// (renewed as `openai` says) and an OpenRouter key.
    fn auth(codex: &str, openai: &str) -> Auth {
        Auth(serde_json::json!({
            "openai-codex": { "type": "oauth", "access": "a", "refresh": codex, "expires": 1, "accountId": "x" },
            "openai": { "type": "oauth", "access": openai, "refresh": openai, "expires": 2, "clientId": "c" },
            "openrouter": { "type": "api_key", "key": "k" },
        }))
    }
}

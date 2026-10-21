// The composer model picker's rows, cached in Filo's database (db.rs): each engine's models as its CLI names them, in
// the picker's order, with the one the CLI answers with by default marked. The picker shows what's here at once; the
// app asks the CLIs again in the background (local_backend.rs `refresh_models`) and saves what they said, so a
// conversation never waits on a CLI and an app restarted shows the same names and defaults straight away.

use rusqlite::params;

use crate::db::Db;
use crate::engine::{self, Engine, ModelOption};

/// The tables (db.rs `MIGRATIONS`).
pub const SCHEMA: &str = "
CREATE TABLE model_options (
    engine     TEXT NOT NULL,
    position   INTEGER NOT NULL,
    id         TEXT NOT NULL,
    label      TEXT NOT NULL,
    is_default INTEGER NOT NULL,
    aliases    TEXT NOT NULL,
    PRIMARY KEY (engine, position)
);
CREATE TABLE model_refreshes (
    engine       TEXT PRIMARY KEY NOT NULL,
    refreshed_at INTEGER NOT NULL
);
";

fn engine_key(engine: Engine) -> &'static str {
    engine.key()
}

/// The rows saved for `engine`, in the picker's order; none before its first refresh.
pub fn cached(db: &Db, engine: Engine) -> Result<Vec<ModelOption>, String> {
    db.with(|conn| {
        let mut statement = conn
            .prepare("SELECT id, label, is_default, aliases FROM model_options WHERE engine = ?1 ORDER BY position")?;
        let rows = statement.query_map(params![engine_key(engine)], |row| {
            let aliases: String = row.get(3)?;
            Ok(ModelOption {
                engine,
                id: row.get(0)?,
                label: row.get(1)?,
                default: row.get::<_, i64>(2)? != 0,
                aliases: serde_json::from_str(&aliases).unwrap_or_default(),
                unavailable: None,
            })
        })?;
        rows.collect()
    })
}

/// When `engine`'s rows were last saved (ms), if ever.
pub fn refreshed_at(db: &Db, engine: Engine) -> Result<Option<i64>, String> {
    use rusqlite::OptionalExtension as _;
    db.with(|conn| {
        conn.query_row(
            "SELECT refreshed_at FROM model_refreshes WHERE engine = ?1",
            params![engine_key(engine)],
            |row| row.get(0),
        )
        .optional()
    })
}

/// Replaces `engine`'s rows with what its CLI said at `now` (ms). Rows of another engine are left as they are.
pub fn save(db: &Db, engine: Engine, rows: &[ModelOption], now: i64) -> Result<(), String> {
    db.write(|tx| {
        tx.execute("DELETE FROM model_options WHERE engine = ?1", params![engine_key(engine)])?;
        for (position, row) in rows.iter().filter(|row| row.engine == engine).enumerate() {
            let aliases = serde_json::to_string(&row.aliases).unwrap_or_else(|_| "[]".into());
            tx.execute(
                "INSERT INTO model_options (engine, position, id, label, is_default, aliases) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![engine_key(engine), position as i64, row.id, row.label, row.default as i64, aliases],
            )?;
        }
        tx.execute(
            "INSERT INTO model_refreshes (engine, refreshed_at) VALUES (?1, ?2) ON CONFLICT(engine) DO UPDATE SET refreshed_at = excluded.refreshed_at",
            params![engine_key(engine), now],
        )?;
        Ok(())
    })
}

/// The picker's rows from the cache: Claude Code's, Codex's, Muse Code's, then OpenCode's. An engine never refreshed yet gets what's known without
/// asking its CLI: Claude Code's aliases under their families' names, Codex's from its own files.
pub fn picker_rows(db: Option<&Db>) -> Vec<ModelOption> {
    let mut rows = Vec::new();
    for engine in Engine::ALL {
        let refreshed = db.is_some_and(|db| refreshed_at(db, engine).ok().flatten().is_some());
        match db.filter(|_| refreshed).and_then(|db| cached(db, engine).ok()) {
            Some(cached) => rows.extend(cached),
            None if engine == Engine::Claude => rows.extend(engine::claude_options(None, &[])),
            None if engine == Engine::Codex => rows.extend(engine::codex_options()),
            // Muse Code and OpenCode say theirs once asked: their own default until then.
            None => {}
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(engine: Engine, id: &str, label: &str, default: bool, aliases: &[&str]) -> ModelOption {
        ModelOption {
            engine,
            id: id.into(),
            label: label.into(),
            default,
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            unavailable: None,
        }
    }

    #[test]
    fn rows_are_kept_in_order_per_engine_across_restarts() {
        let path = crate::db::tests::temp_path("models");
        let db = Db::open(&path).unwrap();
        assert!(cached(&db, Engine::Claude).unwrap().is_empty());
        assert_eq!(refreshed_at(&db, Engine::Claude).unwrap(), None);

        let claude = vec![
            row(Engine::Claude, "fable", "Fable 5.1", false, &["claude-fable-5-1"]),
            row(Engine::Claude, "claude-opus-5-5", "Opus 5.5", true, &["opus"]),
        ];
        save(&db, Engine::Claude, &claude, 1_000).unwrap();
        save(&db, Engine::Codex, &[row(Engine::Codex, "gpt-a", "GPT-A", true, &[])], 2_000).unwrap();
        drop(db);

        // The app opened again: the same rows and defaults, without asking anyone.
        let db = Db::open(&path).unwrap();
        assert_eq!(cached(&db, Engine::Claude).unwrap(), claude);
        assert_eq!(refreshed_at(&db, Engine::Codex).unwrap(), Some(2_000));
        let picker = picker_rows(Some(&db));
        let ids: Vec<&str> = picker.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["fable", "claude-opus-5-5", "gpt-a"]);

        // A refresh replaces one engine's rows, fewer this time, and leaves the other's.
        save(&db, Engine::Claude, &[row(Engine::Claude, "claude-sonnet-5-5", "Sonnet 5.5", true, &["sonnet"])], 3_000)
            .unwrap();
        let picker = picker_rows(Some(&db));
        let rows: Vec<(&str, bool)> = picker.iter().map(|row| (row.label.as_str(), row.default)).collect();
        assert_eq!(rows, [("Sonnet 5.5", true), ("GPT-A", true)]);
        assert_eq!(refreshed_at(&db, Engine::Claude).unwrap(), Some(3_000));
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn an_engine_never_refreshed_shows_what_is_known_without_its_cli() {
        let db = Db::in_memory().unwrap();
        let picker = picker_rows(Some(&db));
        let claude: Vec<(&str, bool)> = picker
            .iter()
            .filter(|row| row.engine == Engine::Claude)
            .map(|row| (row.label.as_str(), row.default))
            .collect();
        assert_eq!(claude, [("Fable", false), ("Opus", false), ("Sonnet", false), ("Haiku", false)]);
        // Refreshed to nothing (Codex's files gone): nothing, rather than guesses.
        save(&db, Engine::Codex, &[], 1).unwrap();
        assert!(picker_rows(Some(&db)).iter().all(|row| row.engine == Engine::Claude));
        assert_eq!(picker_rows(None).iter().filter(|row| row.engine == Engine::Claude).count(), 4);
    }
}

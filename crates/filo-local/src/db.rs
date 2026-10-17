// Filo's database on this Mac: one SQLite file (`<root>/filo.db`, next to the chats) for the pages' data that is rows
// rather than conversations. Notes (notes.rs), memories (memories.rs), the schedule (schedule.rs), coding (coding.rs) and
// coding hand-offs (coding_handoff.rs), the model picker's cache (models.rs), the agents' group chat and its asks
// (group.rs), the Feed (feed.rs), artifacts (artifacts.rs), apps (apps.rs), the glances Filo shows in its chats
// (glances.rs), the optional pages (pages.rs) and the app's own data: agents, profile, read markers and pictures
// (app_data.rs) are here. Skills were too, and are folders beside it now (skills.rs).
//
// Opening it brings the schema up to date: `MIGRATIONS` run in order, each once, and `PRAGMA user_version` records how
// many have run. A page's tables are one more entry at the end; an entry that has shipped never changes, since a Mac
// that ran it won't run it again. A Filo older than the file refuses it (`migrate`) rather than guess at its tables.
// The file is in WAL mode with a busy timeout, so another process (Filo's MCP server, a terminal check) can read and
// write it while the app has it open.
//
// Opening a file also copies in the app's JSON files once (the agents, the profile, read markers and pictures:
// app_data.rs), and keeps a copy of the database a day, and right before a migration or that import (backup.rs).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior};

/// The database's file name, in the local data folder.
pub const FILE: &str = "filo.db";

/// The schema, one step per entry, oldest first. Append only.
const MIGRATIONS: &[&str] = &[
    crate::notes::SCHEMA,
    crate::memories::SCHEMA,
    crate::skills::SCHEMA,
    crate::schedule::SCHEMA,
    crate::coding::SCHEMA,
    crate::models::SCHEMA,
    crate::group::SCHEMA,
    crate::feed::SCHEMA,
    crate::artifacts::SCHEMA,
    crate::apps::SCHEMA,
    crate::group::ASKS_SCHEMA,
    crate::coding_handoff::SCHEMA,
    crate::glances::SCHEMA,
    crate::app_data::SCHEMA,
];

pub struct Db {
    conn: Mutex<Connection>,
    /// The folder the file is in, beside the chats; none in memory.
    dir: Option<PathBuf>,
}

impl Db {
    /// Opens (or makes) the database at `path` and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Db, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("Couldn't make {}: {error}", parent.display()))?;
        }
        let conn = Connection::open(path).map_err(|error| format!("Couldn't open {}: {error}", path.display()))?;
        let dir = path.parent().map(Path::to_path_buf);
        let mut backed_up = false;
        let db = Db::ready(conn, dir.clone(), &mut backed_up)
            .map_err(|error| format!("Couldn't set up {}: {error}", path.display()))?;
        if let Some(dir) = &dir {
            // The JSON files, copied in once; a copy of the database first, unless the migration made one or there's
            // today's (made before anything was copied: while a file can't be read, each open tries again).
            if !crate::app_data::imported(&db)? {
                if !backed_up
                    && crate::app_data::FILES.iter().any(|name| dir.join(name).exists())
                    && let Err(error) = crate::backup::today(&db, false)
                {
                    eprintln!("filo: {error}");
                }
                match crate::app_data::import(&db, dir) {
                    Ok(imported) if !imported.files.is_empty() => {
                        eprintln!(
                            "filo: copied {} into Filo's database ({} agents)",
                            imported.files.join(", "),
                            imported.agents
                        )
                    }
                    Ok(_) => {}
                    Err(error) => eprintln!("filo: the import into Filo's database waits: {error}"),
                }
            }
            // Today's copy, unless there's one.
            if let Err(error) = crate::backup::today(&db, false) {
                eprintln!("filo: {error}");
            }
        }
        Ok(db)
    }

    /// A database that lives only as long as it does (tests).
    #[cfg(any(test, feature = "dev"))]
    pub fn in_memory() -> Result<Db, String> {
        let conn = Connection::open_in_memory().map_err(|error| error.to_string())?;
        Db::ready(conn, None, &mut false).map_err(|error| error.to_string())
    }

    /// Sets the connection up and migrates the file, `backed_up` once it made a copy of it first.
    fn ready(mut conn: Connection, dir: Option<PathBuf>, backed_up: &mut bool) -> rusqlite::Result<Db> {
        conn.busy_timeout(Duration::from_secs(5))?;
        // `journal_mode` answers with the mode it's in (`memory` for an in-memory database), so it's read, not set.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        // A copy of the file as it was, before a migration changes it (not a new one's).
        let done = version(&conn)?;
        if let Some(dir) = dir.as_deref().filter(|_| done > 0 && done < MIGRATIONS.len()) {
            match crate::backup::make(&conn, dir, true) {
                Ok(_) => *backed_up = true,
                Err(error) => eprintln!("filo: {error}"),
            }
        }
        migrate(&mut conn)?;
        // In case another page's migration took the app data's slot (its tables are only made if they're not there).
        conn.execute_batch(crate::app_data::SCHEMA)?;
        // Filo's own chats pinned or archived: nothing showed them, and those chats are no longer read. (Its CREATE
        // left the app data's entry, which an older Filo still runs on each open, so it gets the table back empty.)
        conn.execute_batch("DROP TABLE IF EXISTS chat_marks;")?;
        // Columns added beside the migrations, so a Filo that doesn't know them still opens the file (feed.rs).
        crate::feed::ensure_columns(&conn)?;
        crate::coding_handoff::ensure_columns(&conn)?;
        crate::glances::ensure_columns(&conn)?;
        crate::group::ensure_columns(&conn)?;
        crate::artifacts::ensure_columns(&conn)?;
        crate::group::move_old_speakers(&conn)?;
        Ok(Db { conn: Mutex::new(conn), dir })
    }

    /// The folder the database is in (none in memory), beside the chats.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Runs `read` on the connection; errors come back as the message a page shows.
    pub fn with<T>(&self, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        read(&conn).map_err(failed)
    }

    /// Changes when another connection (Filo's MCP server, a terminal) commits to the file, so the app can tell its
    /// pages; this connection's own writes leave it as it is.
    pub fn data_version(&self) -> Result<i64, String> {
        self.with(|conn| conn.pragma_query_value(None, "data_version", |row| row.get(0)))
    }

    /// Runs `write` in a transaction, committed if it succeeds.
    pub fn write<T>(&self, write: impl FnOnce(&Transaction) -> rusqlite::Result<T>) -> Result<T, String> {
        let mut conn = self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction().map_err(failed)?;
        let result = write(&tx).map_err(failed)?;
        tx.commit().map_err(failed)?;
        Ok(result)
    }
}

fn failed(error: rusqlite::Error) -> String {
    format!("Filo's database: {error}")
}

/// Runs the migrations this file hasn't had yet, each in its own transaction.
fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    if version(conn)? > MIGRATIONS.len() {
        return Err(newer(version(conn)?));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version(conn)?) {
        // Immediate, and the version read again inside: two processes opening the file at once run each step once.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version(&tx)? > index {
            continue;
        }
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

/// How many migrations the file has had.
fn version(conn: &Connection) -> rusqlite::Result<usize> {
    conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).map(|done| done as usize)
}

/// Written by a newer Filo: its tables may not be what this one expects, and its agents may be on engines this one
/// doesn't know, so this one stops rather than run on without them.
fn newer(done: usize) -> rusqlite::Error {
    let message = format!("{NEWER} (schema {done}, this one knows {})", MIGRATIONS.len());
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR), Some(message))
}

/// What the error of a database from a newer Filo starts with (the app stops with it: main.rs).
pub const NEWER: &str = "the database is from a newer Filo";

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("filo-local-test-{name}-{}", uuid::Uuid::new_v4())).join(FILE)
    }

    #[test]
    fn opening_runs_each_migration_once() {
        let path = temp_path("db");
        let db = Db::open(&path).unwrap();
        let version: i64 = db.with(|conn| conn.pragma_query_value(None, "user_version", |row| row.get(0))).unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        drop(db);
        // Opening again finds the schema up to date and leaves it be.
        let db = Db::open(&path).unwrap();
        let tables: i64 = db
            .with(|conn| {
                conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'notes'", [], |row| row.get(0))
            })
            .unwrap();
        assert_eq!(tables, 1);
        let mode: String = db.with(|conn| conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))).unwrap();
        assert_eq!(mode, "wal");
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn tables_whose_migration_slot_was_taken_are_made_as_it_opens() {
        // A development Mac where another page's migration ran in the glances' and the coding hand-offs' slots.
        let path = temp_path("db-slot");
        let db = Db::open(&path).unwrap();
        db.with(|conn| {
            conn.execute_batch(
                "DROP TABLE glances; DROP TABLE coding_asks; DROP TABLE coding_requests; DROP TABLE coding_wakes;",
            )
        })
        .unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        let has = |table: &str| {
            db.with(|conn| {
                conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = 'created_at' OR name = 'at')",
                    [table],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap()
        };
        assert!(["glances", "coding_requests", "coding_asks", "coding_wakes"].into_iter().all(has));
        assert!(
            db.with(|conn| conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_table_info('glances') WHERE name = 'wide')",
                [],
                |row| row.get::<_, bool>(0)
            ))
            .unwrap()
        );
        drop(db);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn another_connections_writes_change_the_data_version() {
        let path = temp_path("db-version");
        let (app, server) = (Db::open(&path).unwrap(), Db::open(&path).unwrap());
        let before = app.data_version().unwrap();
        app.write(|tx| {
            tx.execute("INSERT INTO notes (id, title, body, created_at, updated_at) VALUES ('a', 't', 'b', 1, 1)", [])
        })
        .unwrap();
        assert_eq!(app.data_version().unwrap(), before);
        server.write(|tx| tx.execute("DELETE FROM notes", [])).unwrap();
        assert_ne!(app.data_version().unwrap(), before);
        drop((app, server));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let path = temp_path("db-newer");
        drop(Db::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1).unwrap();
        drop(conn);
        assert!(Db::open(&path).err().unwrap().contains("newer Filo"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_failed_write_is_rolled_back() {
        let db = Db::in_memory().unwrap();
        let result: Result<(), String> = db.write(|tx| {
            tx.execute("INSERT INTO notes (id, title, body, created_at, updated_at) VALUES ('a', 't', 'b', 1, 1)", [])?;
            tx.execute("INSERT INTO notes (id, title, body, created_at, updated_at) VALUES ('a', 't', 'b', 1, 1)", [])?;
            Ok(())
        });
        assert!(result.is_err());
        let count: i64 = db.with(|conn| conn.query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))).unwrap();
        assert_eq!(count, 0);
    }
}

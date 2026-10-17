// Copies of Filo's database, in `backups/` beside it: one a day (the first open of the day makes it), and one right
// before the schema changes, which replaces that day's; the app's JSON files are copied in (app_data.rs) only after
// that day's copy is there. The last
// `KEEP` days are kept. Each is made with `VACUUM INTO`, which reads the database in one transaction (it's in WAL mode,
// so the file alone may not hold the latest writes) and writes a compact copy, smaller than the file itself.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::db::Db;

/// The folder, beside the database.
const DIR: &str = "backups";
/// How many days' copies are kept.
const KEEP: usize = 3;

/// `filo-YYYY-MM-DD.db` for today, this Mac's date.
fn name_for_today() -> String {
    format!("filo-{}.db", chrono::Local::now().format("%Y-%m-%d"))
}

/// Today's copy of `db` (beside it), unless there's one or `replace`; none in memory.
pub fn today(db: &Db, replace: bool) -> Result<Option<PathBuf>, String> {
    let Some(dir) = db.dir() else { return Ok(None) };
    db.with(|conn| Ok(make(conn, dir, replace)))?
}

/// Today's copy into `dir`/backups, unless there's one (or `replace`, right before a change); the copy made, if one was.
pub(crate) fn make(conn: &Connection, dir: &Path, replace: bool) -> Result<Option<PathBuf>, String> {
    let folder = dir.join(DIR);
    let file = folder.join(name_for_today());
    if file.exists() && !replace {
        return Ok(None);
    }
    std::fs::create_dir_all(&folder).map_err(|error| format!("Couldn't make {}: {error}", folder.display()))?;
    // Written beside it first, so a copy cut short is never taken for one.
    let part = folder.join(format!("{}.{}.part", name_for_today(), std::process::id()));
    let _ = std::fs::remove_file(&part);
    let copied = conn
        .execute("VACUUM INTO ?1", [part.to_string_lossy()])
        .map_err(|error| format!("Couldn't back up Filo's database: {error}"));
    if let Err(error) = copied.and_then(|_| {
        std::fs::rename(&part, &file).map_err(|error| format!("Couldn't back up Filo's database: {error}"))
    }) {
        let _ = std::fs::remove_file(&part);
        return Err(error);
    }
    prune(&folder);
    Ok(Some(file))
}

/// Keeps the newest `KEEP` copies (their names sort by day).
fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut copies: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("filo-") && name.ends_with(".db"))
        })
        .collect();
    copies.sort();
    let extra = copies.len().saturating_sub(KEEP);
    for old in &copies[..extra] {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_a_day_the_last_few_kept_and_a_change_replaces_todays() {
        let path = crate::db::tests::temp_path("backup");
        let dir = path.parent().unwrap().to_path_buf();
        let db = Db::open(&path).unwrap();
        let todays = dir.join(DIR).join(name_for_today());
        // Opening made today's copy, a database of its own with what was there.
        assert!(todays.exists());
        db.write(|tx| {
            tx.execute("INSERT INTO notes (id, title, body, created_at, updated_at) VALUES ('a', 't', 'b', 1, 1)", [])
        })
        .unwrap();
        assert_eq!(today(&db, false).unwrap(), None);
        let notes = |file: &Path| {
            Connection::open(file)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(notes(&todays), 0);
        // Right before a change, today's is made again, with the latest writes (still in the WAL).
        assert_eq!(today(&db, true).unwrap().as_deref(), Some(todays.as_path()));
        assert_eq!(notes(&todays), 1);
        // Older days beyond the last `KEEP` go.
        for day in ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-04"] {
            std::fs::write(dir.join(DIR).join(format!("filo-{day}.db")), b"old").unwrap();
        }
        std::fs::write(dir.join(DIR).join("notes.txt"), b"not a copy").unwrap();
        today(&db, true).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(dir.join(DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["filo-2026-01-03.db", "filo-2026-01-04.db", &name_for_today(), "notes.txt"]);
        drop(db);
        std::fs::remove_dir_all(&dir).ok();
    }
}

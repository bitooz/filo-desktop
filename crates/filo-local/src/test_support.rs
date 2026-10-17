// What the crate's tests share: a folder of a test's own, gone when the test ends however it ends (`temp_dir`), a
// stand-in CLI (`script`), and the time zone the schedule's tests read their times in (`tz`).

use std::path::{Path, PathBuf};

/// A test's own folder under $TMPDIR, made empty; removed when it's dropped, passed or failed. Reads as its path.
pub(crate) struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A folder of the test's own, named after `name`.
pub(crate) fn temp_dir(name: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("filo-local-test-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
}

/// An executable script `name` in `dir` (made if it isn't there): a stand-in for a CLI.
pub(crate) fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// UTC+5, the schedule's tests' time zone.
pub(crate) fn tz() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(5 * 3600).unwrap()
}

// Reading a JSON file so it's never mistaken for an empty one (`read_json`), and writing a file whole or not at all: through a hidden temporary file beside it and a rename, so a reader (the app, an
// MCP server, another Filo) never sees half of one and a crash leaves the old one. The temporary file's name is unique
// to the write (a uuid), so two processes or threads writing the same file can't collide, and it's removed when the
// write fails.

use std::fs;
use std::io;
use std::path::Path;

/// A JSON file as `T`: None when there's none (or it's empty), an error when it's there but can't be read: damaged, or
/// written by a newer Filo with a value this build doesn't know. A caller that saves the file again stops on the error,
/// so it never saves over what it couldn't read (the agents.json wipe, 2026-10-14).
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
    match fs::read(path) {
        Ok(bytes) if bytes.trim_ascii().is_empty() => Ok(None),
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| format!("Couldn't read {name}: {error}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Couldn't read {name}: {error}")),
    }
}

/// `bytes` as the whole of the file at `path`, its folder made if missing.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write(path, bytes, None)
}

/// The same, readable by this user only (settings and tokens, the app's config.rs).
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write(path, bytes, Some(0o600))
}

fn write(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a file's path: {}", path.display())));
    };
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(dir)?;
    }
    let temp = dir.join(format!(".{}.{}.part", name.to_string_lossy(), uuid::Uuid::new_v4()));
    let written = fs::write(&temp, bytes).and_then(|()| {
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&temp, path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_replaced_whole_and_leaves_nothing_beside_it() {
        let dir = crate::test_support::temp_dir("files");
        let file = dir.join("deep").join("settings.json");
        write_atomic(&file, b"one").unwrap();
        write_private(&file, b"two").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_dir(dir.join("deep")).unwrap().count(), 1);
        // A folder where the file should be: the write fails, and its temporary file doesn't stay.
        fs::create_dir_all(dir.join("taken")).unwrap();
        assert!(write_atomic(&dir.join("taken"), b"x").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}

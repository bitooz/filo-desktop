// What the end-to-end tests share: a folder of a test's own, a stand-in CLI and the runtime a turn runs with.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use filo_local::turn::Runtime;

/// A fresh folder under $TMPDIR named after `name`.
pub fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("filo-local-e2e-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// The executable script `name` in `bin` (made if it isn't there): a stand-in for a CLI.
pub fn stand_in(bin: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A turn's runtime with this crate's own MCP server program, its CLIs found on `path_env`.
pub fn runtime(path_env: Option<String>, db: Option<PathBuf>, hung_after: Duration) -> Runtime {
    Runtime {
        mcp_program: PathBuf::from(env!("CARGO_BIN_EXE_filo-local")),
        mcp_args: vec!["mcp".into()],
        path_env,
        hung_after,
        db,
        agent: None,
    }
}

/// `bin`, then the system's folders: what a stand-in CLI's turn finds programs on.
pub fn path_with(bin: &Path) -> String {
    format!("{}:/usr/bin:/bin", bin.display())
}

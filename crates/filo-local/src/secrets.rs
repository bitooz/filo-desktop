// The apps' secrets (apps.rs): API keys, sign-in tokens and OAuth client secrets, in the login Keychain as generic
// passwords of the service `im.filo.desktop.apps`, one per app and kind (`<app id>/api-key`, `<app id>/tokens`,
// `<app id>/client-secret`). Filo's database only records that one is there.
//
// The app writes them and the MCP proxy (apps_client.rs), the same binary run by the coding tools, reads them, so
// macOS doesn't ask: a Keychain item trusts the program that made it. A debug build is a new program each time it's
// built, so macOS may ask once after a rebuild ("Always Allow").
//
// Each is kept with where it may go (`set_for`): the origin of the server it's for, or the program a local app's key is
// given to. Anything that can run Filo's own program can point the proxy at a database of its own, so `get_for` refuses
// a secret asked for anywhere else, and a server's address changed in Filo needs its key or sign-in again. A secret
// kept before they were bound is bound where it's first used.
//
// `FILO_SECRETS_DIR` keeps them as files in that folder instead (owner-only): for checks and tests, and only in a `dev`
// build, so the packaged app always uses the Keychain.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "im.filo.desktop.apps";
/// What a bound secret starts with, then the one place it may go and a newline, then the secret itself.
const BOUND: &str = "filo-bound-v1\n";

#[derive(Clone)]
pub enum Secrets {
    Keychain {
        service: String,
    },
    /// Plain files, for checks (`FILO_SECRETS_DIR`).
    Files(PathBuf),
    /// In this process only (unit tests).
    Memory(Arc<Mutex<HashMap<String, String>>>),
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Secrets::Keychain { service } => write!(f, "Keychain({service})"),
            Secrets::Files(dir) => write!(f, "Files({})", dir.display()),
            Secrets::Memory(_) => write!(f, "Memory"),
        }
    }
}

/// The Keychain item's account for an app's secret of `kind`.
pub fn account(app_id: &str, kind: &str) -> String {
    format!("{app_id}/{kind}")
}

impl Secrets {
    /// Where this Mac keeps them: the Keychain, or `FILO_SECRETS_DIR` for checks (`dev` builds).
    pub fn system() -> Secrets {
        Secrets::system_for(SERVICE)
    }

    /// `system`, with the Keychain items of `service` (the browser's logins, browser_import.rs).
    pub fn system_for(service: &str) -> Secrets {
        #[cfg(feature = "dev")]
        if let Some(dir) = std::env::var_os("FILO_SECRETS_DIR").filter(|dir| !dir.is_empty()) {
            return Secrets::Files(PathBuf::from(dir));
        }
        Secrets::Keychain { service: service.into() }
    }

    pub fn memory() -> Secrets {
        Secrets::Memory(Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn get(&self, account: &str) -> Result<Option<String>, String> {
        match self {
            Secrets::Keychain { service } => keychain::get(service, account),
            Secrets::Files(dir) => match std::fs::read_to_string(dir.join(file_name(account))) {
                Ok(text) => Ok(Some(text)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("Couldn't read a secret: {error}")),
            },
            Secrets::Memory(map) => Ok(map.lock().unwrap().get(account).cloned()),
        }
    }

    pub fn set(&self, account: &str, value: &str) -> Result<(), String> {
        match self {
            Secrets::Keychain { service } => keychain::set(service, account, value),
            Secrets::Files(dir) => {
                use std::io::Write as _;
                use std::os::unix::fs::OpenOptionsExt as _;
                std::fs::create_dir_all(dir).map_err(|error| format!("Couldn't keep a secret: {error}"))?;
                let path = dir.join(file_name(account));
                let _ = std::fs::remove_file(&path);
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|error| format!("Couldn't keep a secret: {error}"))?;
                file.write_all(value.as_bytes()).map_err(|error| format!("Couldn't keep a secret: {error}"))
            }
            Secrets::Memory(map) => {
                map.lock().unwrap().insert(account.to_string(), value.to_string());
                Ok(())
            }
        }
    }

    /// The secret `account` keeps, when it was kept for `to` (`set_for`); an error naming where it's for when it wasn't.
    /// One kept before secrets were bound is bound to `to` now.
    pub fn get_for(&self, account: &str, to: &str) -> Result<Option<String>, String> {
        let Some(kept) = self.get(account)? else { return Ok(None) };
        match kept.strip_prefix(BOUND).and_then(|rest| rest.split_once('\n')) {
            Some((bound, secret)) if bound == to => Ok(Some(secret.to_string())),
            Some((bound, _)) => Err(format!(
                "This app's key or sign-in is for {bound}, not {to}. Add it again in Filo (Extensions › Apps) to use it there."
            )),
            None => {
                self.set_for(account, &kept, to)?;
                Ok(Some(kept))
            }
        }
    }

    /// The secret alone, wherever it's for: only to hide it from what Filo shares (the app's result_share.rs), never to
    /// send it.
    pub fn value(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.get(account)?.map(|kept| match kept.strip_prefix(BOUND).and_then(|rest| rest.split_once('\n')) {
            Some((_, secret)) => secret.to_string(),
            None => kept,
        }))
    }

    /// Keeps `secret` for `to` alone: the origin of the server it's sent to, or the program it's given to.
    pub fn set_for(&self, account: &str, secret: &str, to: &str) -> Result<(), String> {
        self.set(account, &format!("{BOUND}{}\n{secret}", to.replace('\n', " ")))
    }

    /// Forgets it; one that isn't there is fine.
    pub fn delete(&self, account: &str) -> Result<(), String> {
        match self {
            Secrets::Keychain { service } => keychain::delete(service, account),
            Secrets::Files(dir) => match std::fs::remove_file(dir.join(file_name(account))) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("Couldn't remove a secret: {error}"))
                }
                _ => Ok(()),
            },
            Secrets::Memory(map) => {
                map.lock().unwrap().remove(account);
                Ok(())
            }
        }
    }
}

fn file_name(account: &str) -> String {
    account.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::passwords;

    /// errSecItemNotFound.
    const NOT_FOUND: i32 = -25300;

    pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
        match passwords::get_generic_password(service, account) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(error) if error.code() == NOT_FOUND => Ok(None),
            Err(error) => Err(format!("Couldn't read the Keychain: {error}")),
        }
    }

    pub fn set(service: &str, account: &str, value: &str) -> Result<(), String> {
        passwords::set_generic_password(service, account, value.as_bytes())
            .map_err(|error| format!("Couldn't save to the Keychain: {error}"))
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        match passwords::delete_generic_password(service, account) {
            Err(error) if error.code() != NOT_FOUND => Err(format!("Couldn't remove it from the Keychain: {error}")),
            _ => Ok(()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    pub fn get(_: &str, _: &str) -> Result<Option<String>, String> {
        Err("There's no Keychain here: set FILO_SECRETS_DIR.".into())
    }
    pub fn set(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("There's no Keychain here: set FILO_SECRETS_DIR.".into())
    }
    pub fn delete(_: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_secrets_are_owner_only_and_forgotten() {
        let dir = crate::test_support::temp_dir("secrets");
        let secrets = Secrets::Files(dir.to_path_buf());
        assert_eq!(secrets.get("a/api-key").unwrap(), None);
        secrets.set("a/api-key", "k1").unwrap();
        secrets.set("a/api-key", "k2").unwrap();
        assert_eq!(secrets.get("a/api-key").unwrap().as_deref(), Some("k2"));
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join("a_api-key")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        secrets.delete("a/api-key").unwrap();
        secrets.delete("a/api-key").unwrap();
        assert_eq!(secrets.get("a/api-key").unwrap(), None);
    }

    /// The login Keychain itself, under a service of its own that's removed after (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn the_keychain_keeps_and_forgets_a_secret() {
        let secrets = Secrets::Keychain { service: format!("im.filo.desktop.apps.test-{}", uuid::Uuid::new_v4()) };
        assert_eq!(secrets.get("a/tokens").unwrap(), None);
        secrets.set("a/tokens", "{\"access_token\":\"t1\"}").unwrap();
        secrets.set("a/tokens", "{\"access_token\":\"t2\"}").unwrap();
        assert_eq!(secrets.get("a/tokens").unwrap().as_deref(), Some("{\"access_token\":\"t2\"}"));
        secrets.delete("a/tokens").unwrap();
        assert_eq!(secrets.get("a/tokens").unwrap(), None);
        secrets.delete("a/tokens").unwrap();
    }
}

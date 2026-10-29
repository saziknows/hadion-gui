//! Tokens and API keys in the operating system's keychain (Windows
//! Credential Manager, macOS Keychain, the Secret Service on Linux), under
//! the service name `hadion` (values stored under `gitvis`, the name before
//! the rename, are read and copied over). Nothing is ever written to
//! Hadion's own settings file, a log or a command line.
//!
//! Keychain calls can block briefly (and may show an OS prompt on macOS):
//! call these from a background task, never from a view.

use crate::{Error, Result};

const SERVICE: &str = "hadion";

/// The service name before the rename from gitvis. A value found only there
/// is copied to [`SERVICE`] on first read; deleting removes both.
const LEGACY_SERVICE: &str = "gitvis";

fn entry_in(service: &str, name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(service, name).map_err(|e| Error::new(format!("Keychain: {e}")))
}

fn entry(name: &str) -> Result<keyring::Entry> {
    entry_in(SERVICE, name)
}

fn read(e: &keyring::Entry) -> Result<Option<String>> {
    match e.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::new(format!("Keychain: {e}"))),
    }
}

/// The stored value, `None` when there is none.
pub fn get(name: &str) -> Result<Option<String>> {
    if let Some(v) = read(&entry(name)?)? {
        return Ok(Some(v));
    }
    let Some(v) = read(&entry_in(LEGACY_SERVICE, name)?)? else {
        return Ok(None);
    };
    // Best effort: the value is returned even if the copy fails.
    let _ = set(name, &v);
    Ok(Some(v))
}

/// Store (or replace) a value.
pub fn set(name: &str, value: &str) -> Result<()> {
    entry(name)?
        .set_password(value)
        .map_err(|e| Error::new(format!("Keychain: {e}")))
}

/// Remove a value (under both service names); removing a missing one is
/// not an error.
pub fn delete(name: &str) -> Result<()> {
    for service in [SERVICE, LEGACY_SERVICE] {
        match entry_in(service, name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(Error::new(format!("Keychain: {e}"))),
        }
    }
    Ok(())
}

/// Is a value stored?
pub fn exists(name: &str) -> bool {
    matches!(get(name), Ok(Some(_)))
}

/// The last four characters, for "key ending in …abcd" displays.
pub fn hint(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "••••".to_owned();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_never_show_short_values() {
        assert_eq!(hint("abc"), "••••");
        assert_eq!(hint("sk-ant-api03-abcdefgh1234"), "…1234");
    }

    #[test]
    #[ignore = "touches the OS keychain; run by hand"]
    fn round_trip() {
        let name = "hadion-test-entry";
        set(name, "value-1").unwrap();
        assert_eq!(get(name).unwrap().as_deref(), Some("value-1"));
        delete(name).unwrap();
        assert_eq!(get(name).unwrap(), None);
    }
}

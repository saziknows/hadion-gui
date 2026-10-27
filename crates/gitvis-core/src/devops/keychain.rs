//! Tokens and API keys in the operating system's keychain (Windows
//! Credential Manager, macOS Keychain, the Secret Service on Linux), under
//! the service name `gitvis`. Nothing is ever written to gitvis's own
//! settings file, a log or a command line.
//!
//! Keychain calls can block briefly (and may show an OS prompt on macOS):
//! call these from a background task, never from a view.

use crate::{Error, Result};

const SERVICE: &str = "gitvis";

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).map_err(|e| Error::new(format!("Keychain: {e}")))
}

/// The stored value, `None` when there is none.
pub fn get(name: &str) -> Result<Option<String>> {
    match entry(name)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::new(format!("Keychain: {e}"))),
    }
}

/// Store (or replace) a value.
pub fn set(name: &str, value: &str) -> Result<()> {
    entry(name)?
        .set_password(value)
        .map_err(|e| Error::new(format!("Keychain: {e}")))
}

/// Remove a value; removing a missing one is not an error.
pub fn delete(name: &str) -> Result<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::new(format!("Keychain: {e}"))),
    }
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
        let name = "gitvis-test-entry";
        set(name, "value-1").unwrap();
        assert_eq!(get(name).unwrap().as_deref(), Some("value-1"));
        delete(name).unwrap();
        assert_eq!(get(name).unwrap(), None);
    }
}

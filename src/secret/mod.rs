//! Keeps the account password in the operating system's credential store: the
//! macOS Keychain, the Secret Service (GNOME Keyring, `KWallet`) on Linux, or
//! the Windows Credential Manager.

mod memory;

use keyring_core::Entry;
use thiserror::Error;

pub use memory::Memory;

#[derive(Debug, Error)]
pub enum Error {
    /// No password is stored for the account.
    #[error("no password stored")]
    NotFound,
    #[error("system keychain: {0}")]
    Keychain(String),
}

impl From<keyring_core::Error> for Error {
    fn from(err: keyring_core::Error) -> Self {
        match err {
            keyring_core::Error::NoEntry => Self::NotFound,
            other => Self::Keychain(other.to_string()),
        }
    }
}

/// Stores one secret per service and user.
pub trait Keyring: Send + Sync {
    /// # Errors
    /// [`Error::NotFound`] when nothing is stored.
    fn get(&self, service: &str, user: &str) -> Result<String, Error>;

    /// # Errors
    /// Fails when the store refuses the password.
    fn set(&self, service: &str, user: &str, password: &str) -> Result<(), Error>;

    /// # Errors
    /// [`Error::NotFound`] when nothing is stored.
    fn delete(&self, service: &str, user: &str) -> Result<(), Error>;
}

/// Names the keychain entry for an IMAP server, e.g. "tamis/imap.mail.me.com".
#[must_use]
pub fn service(host: &str) -> String {
    format!("tamis/{host}")
}

/// The operating system's credential store.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl System {
    /// Connects to the platform's credential store.
    ///
    /// # Errors
    /// Fails when the platform has no usable store.
    pub fn new() -> Result<Self, Error> {
        keyring::Entry::store_status()
            .as_ref()
            .map_err(|err| Error::Keychain(err.to_string()))?;
        Ok(Self)
    }
}

impl Keyring for System {
    fn get(&self, service: &str, user: &str) -> Result<String, Error> {
        Ok(Entry::new(service, user)?.get_password()?)
    }

    fn set(&self, service: &str, user: &str, password: &str) -> Result<(), Error> {
        Ok(Entry::new(service, user)?.set_password(password)?)
    }

    fn delete(&self, service: &str, user: &str) -> Result<(), Error> {
        Ok(Entry::new(service, user)?.delete_credential()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn exercise(ring: &dyn Keyring) {
        const SERVICE: &str = "tamis/imap.example.com";
        const USER: &str = "me@example.com";

        assert!(matches!(ring.get(SERVICE, USER), Err(Error::NotFound)));
        ring.set(SERVICE, USER, "s3cret").unwrap();
        assert_eq!(ring.get(SERVICE, USER).unwrap(), "s3cret");
        ring.delete(SERVICE, USER).unwrap();
        assert!(matches!(ring.delete(SERVICE, USER), Err(Error::NotFound)));
    }

    #[test]
    fn memory_keeps_secrets() {
        exercise(&Memory::default());
    }

    #[test]
    fn system_translates_the_store() {
        // The default store is process-wide, so only this test may set it, and
        // it is the in-process mock, never the real keychain.
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        exercise(&System);
    }

    #[test]
    fn service_names_the_server() {
        assert_eq!(service("imap.mail.me.com"), "tamis/imap.mail.me.com");
    }
}

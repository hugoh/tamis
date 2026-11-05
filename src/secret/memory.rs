use std::collections::HashMap;
use std::sync::Mutex;

use super::{Error, Keyring};

/// An in-process [`Keyring`], for tests.
#[derive(Debug, Default)]
pub struct Memory {
    secrets: Mutex<HashMap<(String, String), String>>,
}

impl Memory {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), String>> {
        self.secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Keyring for Memory {
    fn get(&self, service: &str, user: &str) -> Result<String, Error> {
        self.lock()
            .get(&(service.to_owned(), user.to_owned()))
            .cloned()
            .ok_or(Error::NotFound)
    }

    fn set(&self, service: &str, user: &str, password: &str) -> Result<(), Error> {
        self.lock()
            .insert((service.to_owned(), user.to_owned()), password.to_owned());
        Ok(())
    }

    fn delete(&self, service: &str, user: &str) -> Result<(), Error> {
        self.lock()
            .remove(&(service.to_owned(), user.to_owned()))
            .map(|_| ())
            .ok_or(Error::NotFound)
    }
}

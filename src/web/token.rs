//! Random tokens, and a short-lived table of values they stand for.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

const TOKEN_BYTES: usize = 32;
pub(super) const MAX_ITEMS: usize = 32;

/// 256 random bits as unpadded base64url.
///
/// # Errors
/// When the system has no randomness to give.
pub fn new_token() -> Result<String, getrandom::Error> {
    let mut raw = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut raw)?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

/// Unpadded base64url, how credential IDs are stored.
#[must_use]
pub fn b64(raw: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(raw)
}

/// What is stored in place of a token, so the database never holds one.
#[must_use]
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Values handed out under random keys that expire.
#[derive(Debug)]
pub struct Table<T> {
    items: Mutex<HashMap<String, (T, Instant)>>,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self {
            items: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> Table<T> {
    /// Stores `value` for `ttl` and returns its key. When the table is full it
    /// drops the entry that expires first, so unauthenticated callers can't
    /// lock everyone else out.
    ///
    /// # Errors
    /// When a key can't be generated.
    pub fn put(&self, value: T, ttl: Duration) -> Result<String, getrandom::Error> {
        let key = new_token()?;
        let now = Instant::now();
        let mut items = self.lock();
        items.retain(|_, (_, expires)| *expires > now);
        if items.len() >= MAX_ITEMS {
            let oldest = items
                .iter()
                .min_by_key(|(_, (_, expires))| *expires)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                items.remove(&oldest);
            }
        }
        items.insert(key.clone(), (value, now + ttl));
        Ok(key)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<T>
    where
        T: Clone,
    {
        self.lock()
            .get(key)
            .filter(|(_, expires)| *expires > Instant::now())
            .map(|(value, _)| value.clone())
    }

    /// Removes a value and returns it unless it expired, so it can be used
    /// once.
    #[must_use]
    pub fn take(&self, key: &str) -> Option<T> {
        self.lock()
            .remove(key)
            .filter(|(_, expires)| *expires > Instant::now())
            .map(|(value, _)| value)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (T, Instant)>> {
        self.items
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_url_safe() {
        let (first, second) = (new_token().unwrap(), new_token().unwrap());

        assert_ne!(first, second);
        assert!(first.len() >= 43 && !first.contains('='), "{first}");
    }

    #[test]
    fn hashing_is_stable_and_hides_the_token() {
        let hash = hash_token("abc");

        assert_eq!(hash, hash_token("abc"));
        assert_ne!(hash, hash_token("abx"));
        assert!(!hash.contains("abc") && hash.len() == 64);
    }

    #[test]
    fn a_value_is_found_until_it_is_taken_or_expires() {
        let table = Table::default();
        let key = table.put(7, Duration::from_secs(60)).unwrap();

        assert_eq!(table.get(&key), Some(7));
        assert_eq!(table.take(&key), Some(7));
        assert_eq!(table.get(&key), None);

        let gone = table.put(8, Duration::ZERO).unwrap();
        assert_eq!(table.take(&gone), None);
    }

    #[test]
    fn an_evicting_put_replaces_the_oldest_when_full() {
        let table = Table::default();
        let first = table.put(0, Duration::from_secs(60)).unwrap();
        for n in 1..MAX_ITEMS {
            table.put(n, Duration::from_secs(60)).unwrap();
        }

        let newest = table.put(0, Duration::from_secs(60)).unwrap();

        assert_eq!(table.get(&first), None);
        assert!(table.get(&newest).is_some());
    }
}

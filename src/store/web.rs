use jiff::Timestamp;
use rusqlite::{TransactionBehavior, params};

use super::{Error, Result, Store, unix};

/// A registered `WebAuthn` credential. The credential is opaque to the store:
/// the web layer serializes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Passkey {
    pub id: i64,
    pub credential_id: String,
    pub name: String,
    pub credential: Vec<u8>,
    pub created_at: Timestamp,
    pub last_used: Option<Timestamp>,
}

impl Store {
    /// Registers a credential.
    ///
    /// # Errors
    /// [`Error::Exists`] when `credential_id` is already registered.
    pub fn add_passkey(&self, credential_id: &str, name: &str, credential: &[u8]) -> Result<()> {
        let added = self.conn().execute(
            "INSERT INTO passkeys (credential_id, name, credential, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(credential_id) DO NOTHING",
            params![
                credential_id,
                name,
                credential,
                Timestamp::now().as_second()
            ],
        )?;
        if added == 0 {
            return Err(Error::Exists(format!("passkey {name}")));
        }
        Ok(())
    }

    /// The registered credentials, oldest first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn passkeys(&self) -> Result<Vec<Passkey>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, credential_id, name, credential, created_at, last_used FROM passkeys ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            let last_used: i64 = row.get(5)?;
            Ok(Passkey {
                id: row.get(0)?,
                credential_id: row.get(1)?,
                name: row.get(2)?,
                credential: row.get(3)?,
                created_at: unix(row.get(4)?),
                last_used: (last_used != 0).then(|| unix(last_used)),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Stores the credential's new state after a login (its sign counter
    /// changes) and stamps the time of use.
    ///
    /// # Errors
    /// [`Error::NotFound`] when the credential isn't registered.
    pub fn update_passkey(&self, credential_id: &str, credential: &[u8]) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE passkeys SET credential = ?, last_used = ? WHERE credential_id = ?",
            params![credential, Timestamp::now().as_second(), credential_id],
        )?;
        found_one(changed, &format!("passkey {credential_id}"))
    }

    /// Deletes the passkey with the given id.
    ///
    /// # Errors
    /// [`Error::NotFound`] when there is no such passkey.
    pub fn remove_passkey(&self, key_id: i64) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM passkeys WHERE id = ?", [key_id])?;
        found_one(changed, &format!("passkey {key_id}"))
    }

    /// Stores the hash of a one-time enrollment token, and drops tokens that
    /// have already expired.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn add_enrollment(&self, token_hash: &str, expires: Timestamp) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM enrollments WHERE expires_at <= ?",
            [Timestamp::now().as_second()],
        )?;
        tx.execute(
            "INSERT INTO enrollments (token_hash, expires_at) VALUES (?, ?)",
            params![token_hash, expires.as_second()],
        )?;
        Ok(tx.commit()?)
    }

    /// Uses up a token: reports whether `token_hash` was stored and unexpired
    /// at `now`. A token works once.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn consume_enrollment(&self, token_hash: &str, now: Timestamp) -> Result<bool> {
        let removed = self.conn().execute(
            "DELETE FROM enrollments WHERE token_hash = ? AND expires_at > ?",
            params![token_hash, now.as_second()],
        )?;
        Ok(removed > 0)
    }
}

fn found_one(changed: usize, what: &str) -> Result<()> {
    if changed == 0 {
        Err(Error::NotFound(what.to_owned()))
    } else {
        Ok(())
    }
}

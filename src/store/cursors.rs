//! Account settings and per-folder sync state.

use rusqlite::{OptionalExtension, params};

use super::{Result, Store, count};

impl Store {
    /// A value tamis learnt about the account, if it has one.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Saves a value tamis learnt about the account.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// The folder's cursor, if one was saved.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn cursor(&self, mailbox: &str) -> Result<Option<Cursor>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT uid_validity, last_uid FROM cursors WHERE mailbox = ?",
                [mailbox],
                |row| {
                    Ok(Cursor {
                        uid_validity: row.get(0)?,
                        last_uid: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Saves the folder's cursor.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn set_cursor(&self, mailbox: &str, cursor: Cursor) -> Result<()> {
        self.conn().execute(
            "INSERT INTO cursors (mailbox, uid_validity, last_uid) VALUES (?, ?, ?)
             ON CONFLICT(mailbox) DO UPDATE SET
             uid_validity = excluded.uid_validity, last_uid = excluded.last_uid",
            params![mailbox, cursor.uid_validity, cursor.last_uid],
        )?;
        Ok(())
    }

    /// The folder's saved mod-sequence, if one was saved.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn mod_seq(&self, mailbox: &str) -> Result<Option<ModSeq>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT uid_validity, highest FROM mod_seqs WHERE mailbox = ?",
                [mailbox],
                |row| {
                    Ok(ModSeq {
                        uid_validity: row.get(0)?,
                        highest: count(row, 1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Saves the folder's mod-sequence.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn set_mod_seq(&self, mailbox: &str, mod_seq: ModSeq) -> Result<()> {
        self.conn().execute(
            "INSERT INTO mod_seqs (mailbox, uid_validity, highest) VALUES (?, ?, ?)
             ON CONFLICT(mailbox) DO UPDATE SET
             uid_validity = excluded.uid_validity, highest = excluded.highest",
            params![
                mailbox,
                mod_seq.uid_validity,
                i64::try_from(mod_seq.highest).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(())
    }
}

/// How far a folder has been processed: the highest UID seen, valid only
/// while the folder's `UIDVALIDITY` is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub uid_validity: u32,
    pub last_uid: u32,
}

/// A folder's CONDSTORE `HIGHESTMODSEQ` as of the last keyword search, valid
/// only while the folder's `UIDVALIDITY` is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModSeq {
    pub uid_validity: u32,
    pub highest: u64,
}

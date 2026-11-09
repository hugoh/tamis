//! Token statistics: training messages and loading the counts a classifier
//! needs.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Result, Store, count};

/// The label and tokens a message was trained with, if it was.
fn stored_training(conn: &Connection, key: &str) -> Result<Option<(i64, String)>> {
    Ok(conn
        .query_row(
            "SELECT label, tokens FROM trained WHERE key = ?",
            [key],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?)
}
use crate::classify::{FIRST_TOKENIZER_VERSION, Label, Model, TOKENIZER_VERSION};

/// The store key for the tokenizer version the counts were learnt with.
pub const TOKENIZER_SETTING: &str = "tokenizer_version";

impl Store {
    /// Records the message identified by `key` as `label`. Training a message
    /// again with the same label does nothing; with the other label it
    /// reverses the earlier training first. Reports whether anything changed.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn train(&self, key: &str, tokens: &[String], label: Label) -> Result<bool> {
        self.in_tx(|tx| {
            let previous = stored_training(tx, key)?;

            if let Some((old_label, old_tokens)) = previous {
                if old_label == label_to_int(label) {
                    return Ok(false);
                }
                let old_tokens: Vec<String> = serde_json::from_str(&old_tokens)?;
                adjust(tx, &old_tokens, label_from_int(old_label), -1)?;
            }

            // The first message ever learnt fixes the version: later changes
            // to the tokenizer don't touch counts that were already learnt.
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value)
                 SELECT ?, ? WHERE NOT EXISTS (SELECT 1 FROM trained)",
                params![TOKENIZER_SETTING, TOKENIZER_VERSION.to_string()],
            )?;
            adjust(tx, tokens, label, 1)?;
            tx.execute(
                "INSERT INTO trained (key, label, tokens) VALUES (?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET label = excluded.label, tokens = excluded.tokens",
                params![key, label_to_int(label), serde_json::to_string(tokens)?],
            )?;
            Ok(true)
        })
    }

    /// Reverses the training of the message identified by `key`, and reports
    /// whether it had been trained.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn untrain(&self, key: &str) -> Result<bool> {
        self.in_tx(|tx| {
            let stored = stored_training(tx, key)?;
            let Some((label, tokens)) = stored else {
                return Ok(false);
            };

            let tokens: Vec<String> = serde_json::from_str(&tokens)?;
            adjust(tx, &tokens, label_from_int(label), -1)?;
            tx.execute("DELETE FROM trained WHERE key = ?", [key])?;
            Ok(true)
        })
    }

    /// The label a message was trained with, if it was.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn trained(&self, key: &str) -> Result<Option<Label>> {
        let label = self
            .conn()
            .query_row("SELECT label FROM trained WHERE key = ?", [key], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        Ok(label.map(label_from_int))
    }

    /// The tokenizer version the counts were learnt with. A store with no
    /// training counts as current; one trained before versions were recorded
    /// as the first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn tokenizer_version(&self) -> Result<u32> {
        if let Some(stamp) = self.setting(TOKENIZER_SETTING)?
            && let Ok(version) = stamp.parse()
        {
            return Ok(version);
        }
        let trained: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM trained", [], |row| row.get(0))?;
        Ok(if trained == 0 {
            TOKENIZER_VERSION
        } else {
            FIRST_TOKENIZER_VERSION
        })
    }

    /// Records the tokenizer version the counts were learnt with, once they
    /// have all been learnt again.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn set_tokenizer_version(&self, version: u32) -> Result<()> {
        self.set_setting(TOKENIZER_SETTING, &version.to_string())
    }

    /// The statistics of the given tokens, for scoring.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn model(&self, tokens: &[String]) -> Result<Snapshot> {
        let conn = self.conn();
        let (spam, good) = conn.query_row(
            "SELECT COALESCE(SUM(label = ?), 0), COALESCE(SUM(label = ?), 0) FROM trained",
            params![label_to_int(Label::Spam), label_to_int(Label::Good)],
            |row| Ok((count(row, 0)?, count(row, 1)?)),
        )?;

        let mut snapshot = Snapshot {
            counts: HashMap::with_capacity(tokens.len()),
            total_spam: spam,
            total_good: good,
        };
        let mut statement = conn.prepare(
            "SELECT token, spam, good FROM tokens WHERE token IN (SELECT value FROM json_each(?))",
        )?;
        let rows = statement.query_map([serde_json::to_string(tokens)?], |row| {
            Ok((row.get::<_, String>(0)?, (count(row, 1)?, count(row, 2)?)))
        })?;
        for row in rows {
            let (token, counts) = row?;
            snapshot.counts.insert(token, counts);
        }
        Ok(snapshot)
    }
}

/// A [`Model`] loaded from the store for a set of tokens.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    counts: HashMap<String, (u64, u64)>,
    total_spam: u64,
    total_good: u64,
}

impl Model for Snapshot {
    fn counts(&self, token: &str) -> (u64, u64) {
        self.counts.get(token).copied().unwrap_or_default()
    }

    fn totals(&self) -> (u64, u64) {
        (self.total_spam, self.total_good)
    }
}

/// Labels are stored as integers: good is 0, spam is 1.
fn label_to_int(label: Label) -> i64 {
    match label {
        Label::Good => 0,
        Label::Spam => 1,
    }
}

fn label_from_int(value: i64) -> Label {
    if value == 1 { Label::Spam } else { Label::Good }
}

fn adjust(tx: &Transaction<'_>, tokens: &[String], label: Label, delta: i64) -> Result<()> {
    let (spam, good) = if label == Label::Spam {
        (delta, 0)
    } else {
        (0, delta)
    };
    let encoded = serde_json::to_string(tokens)?;

    tx.execute(
        "INSERT INTO tokens (token, spam, good)
         SELECT value, ?, ? FROM json_each(?) WHERE true
         ON CONFLICT(token) DO UPDATE SET
         spam = spam + excluded.spam, good = good + excluded.good",
        params![spam, good, encoded],
    )?;

    if delta < 0 {
        tx.execute(
            "DELETE FROM tokens WHERE spam <= 0 AND good <= 0 AND token IN (SELECT value FROM json_each(?))",
            [encoded],
        )?;
    }
    Ok(())
}

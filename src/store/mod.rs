//! Keeps tamis's state in `SQLite`: token statistics, the whitelist,
//! per-folder sync cursors and the decision journal.

mod addresses;
mod decisions;
mod schema;
mod web;

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use thiserror::Error;

pub use addresses::{Address, AddressQuery, AddressSort, parse_address};
pub use decisions::{Decision, DecisionQuery, DecisionSort, PendingMove, Stats};
pub use web::Passkey;

use crate::classify::{Label, Model};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MIGRATION_ATTEMPTS: u32 = 20;
const MIGRATION_BACKOFF: Duration = Duration::from_millis(50);
const DEFAULT_QUERY_LIMIT: i64 = 100;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}: not found")]
    NotFound(String),
    #[error("decision {0}: already undone")]
    AlreadyUndone(i64),
    #[error("{0}: already exists")]
    Exists(String),
    #[error("invalid address {0:?}")]
    InvalidAddress(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("encode tokens: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// An open state database. One connection serves every caller, one at a time:
/// `SQLite` allows a single writer, and the calls are short.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens or creates the database at `path`, readable only by its owner.
    ///
    /// # Errors
    /// Fails when the file can't be created or the schema can't be brought up
    /// to date.
    pub fn open(path: &Path) -> Result<Self> {
        create_private(path)?;

        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate_with_retry()?;
        Ok(store)
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `body` in a transaction that takes the write lock up front, so two
    /// processes migrating a new database can't both decide the schema is
    /// missing.
    fn in_tx<T>(&self, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = body(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    /// Retries while another process creates the database: `SQLite` reports
    /// `SQLITE_BUSY` at once, without waiting, while a new file is switched to
    /// WAL mode.
    fn migrate_with_retry(&self) -> Result<()> {
        let mut attempt = 1;
        loop {
            match self.migrate() {
                Err(Error::Sql(rusqlite::Error::SqliteFailure(err, _)))
                    if err.code == ErrorCode::DatabaseBusy && attempt < MIGRATION_ATTEMPTS =>
                {
                    attempt += 1;
                    thread::sleep(MIGRATION_BACKOFF);
                }
                other => return other,
            }
        }
    }

    fn migrate(&self) -> Result<()> {
        self.in_tx(|tx| {
            let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
            let version = usize::try_from(version).unwrap_or_default();
            let steps = schema::MIGRATIONS;
            if version >= steps.len() {
                return Ok(());
            }
            for step in &steps[version..] {
                tx.execute_batch(step)?;
            }
            tx.execute_batch(&format!("PRAGMA user_version = {}", steps.len()))?;
            Ok(())
        })
    }

    /// Records the message identified by `key` as `label`. Training a message
    /// again with the same label does nothing; with the other label it
    /// reverses the earlier training first. Reports whether anything changed.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn train(&self, key: &str, tokens: &[String], label: Label) -> Result<bool> {
        self.in_tx(|tx| {
            let previous = tx
                .query_row(
                    "SELECT label, tokens FROM trained WHERE key = ?",
                    [key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;

            if let Some((old_label, old_tokens)) = previous {
                if old_label == label_to_int(label) {
                    return Ok(false);
                }
                let old_tokens: Vec<String> = serde_json::from_str(&old_tokens)?;
                adjust(tx, &old_tokens, label_from_int(old_label), -1)?;
            }

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
            let stored = tx
                .query_row(
                    "SELECT label, tokens FROM trained WHERE key = ?",
                    [key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
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

/// A non-negative integer column. `SQLite` integers are signed; counts and
/// mod-sequences never reach the sign bit.
fn count(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    Ok(u64::try_from(row.get::<_, i64>(index)?).unwrap_or_default())
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

/// Makes sure the database file exists and only its owner can read it: it
/// holds sender addresses and subjects. `SQLite` gives its -wal and -shm files
/// the database file's permissions.
fn create_private(path: &Path) -> Result<()> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(io)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
    }
    Ok(())
}

fn unix(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).unwrap_or(Timestamp::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, store)
    }

    fn tokens(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }

    fn decision(message_id: &str, reason: &str, seconds: i64) -> Decision {
        Decision {
            at: at(seconds),
            message_id: message_id.to_owned(),
            reason: reason.parse().unwrap(),
            verdict: crate::journal::Verdict::Good,
            ..Decision::default()
        }
    }

    #[test]
    fn training_counts_once_and_reverses_on_the_other_label() {
        let (_dir, store) = open();
        let words = tokens(&["free", "prize"]);

        assert!(store.train("m1", &words, Label::Spam).unwrap());
        assert!(
            !store.train("m1", &words, Label::Spam).unwrap(),
            "same label twice does nothing"
        );
        let model = store.model(&words).unwrap();
        assert_eq!((model.totals(), model.counts("free")), ((1, 0), (1, 0)));

        assert!(
            store.train("m1", &words, Label::Good).unwrap(),
            "the other label reverses the first"
        );
        let model = store.model(&words).unwrap();
        assert_eq!((model.totals(), model.counts("free")), ((0, 1), (0, 1)));
        assert_eq!(store.trained("m1").unwrap(), Some(Label::Good));
    }

    #[test]
    fn untraining_forgets_the_message_and_its_unused_tokens() {
        let (_dir, store) = open();
        store
            .train("m1", &tokens(&["only-here"]), Label::Spam)
            .unwrap();

        assert!(store.untrain("m1").unwrap());
        assert!(!store.untrain("m1").unwrap());
        assert_eq!(store.trained("m1").unwrap(), None);
        assert_eq!(store.stats().unwrap().tokens, 0);
    }

    #[test]
    fn bookkeeping_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");

        let store = Store::open(&path).unwrap();
        store.set_setting("k", "v").unwrap();
        store
            .set_cursor(
                "INBOX",
                Cursor {
                    uid_validity: 1,
                    last_uid: 2,
                },
            )
            .unwrap();
        store
            .set_cursor(
                "INBOX",
                Cursor {
                    uid_validity: 3,
                    last_uid: 4,
                },
            )
            .unwrap();
        store
            .set_mod_seq(
                "Junk",
                ModSeq {
                    uid_validity: 1,
                    highest: u64::from(u32::MAX) + 1,
                },
            )
            .unwrap();
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(store.setting("k").unwrap().as_deref(), Some("v"));
        assert_eq!(store.setting("missing").unwrap(), None);
        assert_eq!(
            store.cursor("INBOX").unwrap(),
            Some(Cursor {
                uid_validity: 3,
                last_uid: 4
            })
        );
        assert_eq!(
            store.mod_seq("Junk").unwrap(),
            Some(ModSeq {
                uid_validity: 1,
                highest: u64::from(u32::MAX) + 1
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_database_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        fs::write(&path, b"").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let _store = Store::open(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_missing_directory_is_an_io_error() {
        let err = Store::open(Path::new("/nonexistent/dir/state.db")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    #[test]
    fn decisions_link_corrections_and_undos() {
        let (_dir, store) = open();
        let first = store
            .record_decision(&decision("<a>", "bayes", 10))
            .unwrap();
        let correction = store
            .record_decision(&decision("<a>", "correction", 20))
            .unwrap();
        let unrelated = store
            .record_decision(&decision("", "correction", 30))
            .unwrap();

        let got = store.decision(first).unwrap();
        assert_eq!((got.corrected_by, got.undone_by), (correction, 0));
        assert_eq!(store.decision(correction).unwrap().corrects, [first]);
        assert_eq!(
            store.decision(unrelated).unwrap().corrects,
            Vec::<i64>::new()
        );

        assert_eq!(
            store.latest_decision("<a>").unwrap().unwrap().id,
            correction
        );
        assert_eq!(store.latest_decision("").unwrap(), None);
        assert_eq!(store.latest_decision("<none>").unwrap(), None);
    }

    #[test]
    fn marking_undone_reports_missing_and_repeated() {
        let (_dir, store) = open();
        let id = store
            .record_decision(&decision("<a>", "bayes", 10))
            .unwrap();

        store.mark_undone(id).unwrap();
        assert!(store.decision(id).unwrap().undone);
        assert!(matches!(store.mark_undone(id), Err(Error::AlreadyUndone(got)) if got == id));
        assert!(matches!(store.mark_undone(id + 1), Err(Error::NotFound(_))));
        assert!(matches!(store.decision(id + 1), Err(Error::NotFound(_))));
    }

    #[test]
    fn a_pending_move_is_confirmed_or_dropped() {
        let (_dir, store) = open();
        let moved = store
            .record_pending_move(&decision("<a>", "bayes", 10), "Junk", "$TamisFiled")
            .unwrap();
        let refused = store
            .record_pending_move(&decision("<b>", "bayes", 20), "Junk", "$TamisFiled")
            .unwrap();

        assert!(!store.decision(moved).unwrap().applied);
        let pending = store.pending_moves().unwrap();
        assert_eq!(
            pending.iter().map(|p| p.decision_id).collect::<Vec<_>>(),
            [moved, refused]
        );
        assert_eq!(pending[0].dest, "Junk");
        assert_eq!(pending[0].message_id, "<a>");

        store.confirm_move(moved).unwrap();
        store.drop_move(refused).unwrap();

        assert!(store.decision(moved).unwrap().applied);
        assert!(matches!(store.decision(refused), Err(Error::NotFound(_))));
        assert!(store.pending_moves().unwrap().is_empty());
    }

    #[test]
    fn the_reported_size_includes_the_write_ahead_log() {
        let (dir, store) = open();
        store
            .train("<a>", &["word".to_owned()], Label::Spam)
            .unwrap();
        let on_disk = |suffix: &str| {
            let name = format!("{}{suffix}", dir.path().join("state.db").display());
            std::fs::metadata(name).map_or(0, |m| i64::try_from(m.len()).unwrap())
        };
        assert!(on_disk("-wal") > 0, "the test needs a log to count");

        assert!(store.stats().unwrap().bytes >= on_disk("") + on_disk("-wal"));
    }

    #[test]
    fn pruning_drops_only_older_decisions() {
        let (_dir, store) = open();
        for seconds in [10, 20, 30] {
            store
                .record_decision(&decision("<a>", "bayes", seconds))
                .unwrap();
        }

        assert_eq!(store.prune_decisions(at(25)).unwrap(), 2);
        let stats = store.stats().unwrap();
        assert_eq!((stats.decisions, stats.oldest_decision), (1, Some(at(30))));
    }

    #[test]
    fn the_whitelist_ignores_case() {
        let (_dir, store) = open();
        store.add_address("Alice@Example.org", "sent").unwrap();
        store.add_address("alice@example.org", "again").unwrap();
        store.add_address("bob@example.org", "sent").unwrap();
        store.add_address("100%@example.org", "sent").unwrap();

        assert!(store.has_address("ALICE@example.org").unwrap());
        assert_eq!(store.addresses().unwrap().len(), 3);
        assert_eq!(
            store.addresses().unwrap()[1].source,
            "sent",
            "the first source is kept"
        );

        assert_eq!(
            store
                .remove_addresses(&["BOB@example.org".to_owned(), "nobody@x".to_owned()])
                .unwrap(),
            1
        );
        assert_eq!(
            store.remove_addresses_matching("%").unwrap(),
            1,
            "% is literal"
        );
        store.remove_address("alice@EXAMPLE.org").unwrap();
        assert!(!store.has_address("alice@example.org").unwrap());

        store.add_address("a@x", "s").unwrap();
        store.add_address("b@x", "s").unwrap();
        assert_eq!(store.remove_addresses_matching("").unwrap(), 2);
    }

    #[test]
    fn addresses_parse_with_or_without_a_name() {
        let ok = [
            ("alice@example.org", "alice@example.org"),
            ("  Alice@Example.ORG ", "alice@example.org"),
            ("Alice Smith <alice@example.org>", "alice@example.org"),
            ("\"Smith, Alice\" <Alice@example.org>", "alice@example.org"),
        ];
        for (input, want) in ok {
            assert_eq!(parse_address(input).unwrap(), want, "{input}");
        }

        for input in ["", "not an address", "a@", "@b.c", "a@b.c, d@e.f"] {
            assert!(
                matches!(parse_address(input), Err(Error::InvalidAddress(_))),
                "{input:?}"
            );
        }
    }

    #[test]
    fn passkeys_register_update_and_remove() {
        let (_dir, store) = open();
        store.add_passkey("cred", "laptop", &[1, 2]).unwrap();
        assert!(matches!(
            store.add_passkey("cred", "again", &[3]),
            Err(Error::Exists(_))
        ));

        assert_eq!(store.passkeys().unwrap()[0].last_used, None);
        store.update_passkey("cred", &[9]).unwrap();
        let key = store.passkeys().unwrap().remove(0);
        assert_eq!(key.credential, [9]);
        assert!(key.last_used.is_some());
        assert!(matches!(
            store.update_passkey("other", &[1]),
            Err(Error::NotFound(_))
        ));

        store.remove_passkey(key.id).unwrap();
        assert!(matches!(
            store.remove_passkey(key.id),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn enrollment_tokens_work_once_and_expire() {
        let (_dir, store) = open();
        store.add_enrollment("old", at(1_000)).unwrap();
        store.add_enrollment("fresh", at(4_000_000_000)).unwrap();

        assert!(
            !store.consume_enrollment("old", at(2_000)).unwrap(),
            "expired; add_enrollment dropped it"
        );
        assert!(
            !store
                .consume_enrollment("fresh", at(4_000_000_001))
                .unwrap(),
            "expired by then"
        );
        assert!(store.consume_enrollment("fresh", at(2_000)).unwrap());
        assert!(
            !store.consume_enrollment("fresh", at(2_000)).unwrap(),
            "a token works once"
        );
    }

    #[test]
    fn a_journal_row_with_an_unknown_word_is_an_error_not_a_guess() {
        let (_dir, store) = open();
        store
            .record_decision(&decision("<a>", "bayes", 10))
            .unwrap();
        store
            .conn()
            .execute("UPDATE decisions SET verdict = 'maybe'", [])
            .unwrap();

        let err = store.decisions(10).unwrap_err().to_string();

        assert!(err.contains("maybe"), "{err}");
    }
}

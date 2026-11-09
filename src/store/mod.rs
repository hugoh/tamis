//! Keeps tamis's state in `SQLite`: token statistics, the whitelist,
//! per-folder sync cursors and the decision journal.

mod addresses;
mod cursors;
mod decisions;
mod model;
mod schema;
mod web;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use rusqlite::types::Value;
use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior};
use thiserror::Error;

pub use addresses::{
    Address, AddressQuery, AddressSort, MANUAL_SOURCE, parse_address, parse_addresses,
};
pub use cursors::{Cursor, ModSeq};
pub use decisions::{Decision, DecisionQuery, DecisionSort, PendingMarks, PendingMove, Stats};
pub use model::Snapshot;
pub use web::Passkey;

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
    #[error("{0} is not whitelisted")]
    NotWhitelisted(String),
    #[error("select at least one address")]
    NoAddresses,
    #[error("no addresses match")]
    NoMatch,
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("another tamis is already running on {0}")]
    AlreadyRunning(PathBuf),
    #[error(
        "database is from a newer tamis (schema {found}, this build knows {known}): upgrade tamis"
    )]
    NewerSchema { found: usize, known: usize },
    #[error("database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("encode tokens: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Whether the error is the caller's input to fix, not a fault.
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidAddress(_) | Self::NoAddresses | Self::NoMatch | Self::NotWhitelisted(_)
        )
    }
}

/// Appends the `LIMIT` argument of a paged query; a limit that isn't positive
/// means the default.
fn push_page(args: &mut Vec<Value>, limit: i64) {
    let limit = if limit <= 0 {
        DEFAULT_QUERY_LIMIT
    } else {
        limit
    };
    args.push(Value::Integer(limit));
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

    /// Folds the write-ahead log into the database file and empties it, as a
    /// clean shutdown should.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn()
            .pragma_update_and_check(None, "wal_checkpoint", "TRUNCATE", |_| Ok(()))?;
        Ok(())
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
            if version > steps.len() {
                return Err(Error::NewerSchema {
                    found: version,
                    known: steps.len(),
                });
            }
            if version == steps.len() {
                return Ok(());
            }
            for step in &steps[version..] {
                tx.execute_batch(step)?;
            }
            tx.execute_batch(&format!("PRAGMA user_version = {}", steps.len()))?;
            Ok(())
        })
    }
}

/// A non-negative integer column. `SQLite` integers are signed; counts and
/// mod-sequences never reach the sign bit.
fn count(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    Ok(u64::try_from(row.get::<_, i64>(index)?).unwrap_or_default())
}

/// Takes the lock that keeps a second `tamis run` off the database at `path`:
/// two daemons would judge and move the same mail. It lasts until the file is
/// dropped or the process ends.
///
/// # Errors
/// [`Error::AlreadyRunning`] when another process holds it.
pub fn lock_instance(path: &Path) -> Result<File> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let io = |source| Error::Io {
        path: lock_path.clone(),
        source,
    };

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::AlreadyRunning(path.to_path_buf())),
        Err(TryLockError::Error(source)) => Err(io(source)),
    }
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
    use crate::classify::{Label, Model};

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
    fn a_checkpoint_empties_the_wal() {
        let (dir, store) = open();
        store.set_setting("k", "v").unwrap();
        let wal = dir.path().join("state.db-wal");
        assert!(fs::metadata(&wal).unwrap().len() > 0);

        store.checkpoint().unwrap();

        assert_eq!(fs::metadata(&wal).unwrap().len(), 0);
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
    fn a_new_store_counts_under_the_current_tokenizer() {
        let (_dir, store) = open();
        assert_eq!(
            store.tokenizer_version().unwrap(),
            crate::classify::TOKENIZER_VERSION
        );

        store.train("m1", &tokens(&["free"]), Label::Spam).unwrap();

        assert_eq!(
            store.tokenizer_version().unwrap(),
            crate::classify::TOKENIZER_VERSION
        );
    }

    #[test]
    fn counts_learnt_before_versions_were_recorded_are_the_first_tokenizer() {
        let (_dir, store) = open();
        store
            .conn()
            .execute(
                "INSERT INTO trained (key, label, tokens) VALUES ('m1', 1, '[\"free\"]')",
                [],
            )
            .unwrap();

        assert_eq!(store.tokenizer_version().unwrap(), 1);
    }

    #[test]
    fn training_leaves_an_older_stamp_alone_until_the_counts_are_refreshed() {
        let (_dir, store) = open();
        store.train("m1", &tokens(&["free"]), Label::Spam).unwrap();
        store.set_tokenizer_version(1).unwrap();

        store.train("m2", &tokens(&["prize"]), Label::Spam).unwrap();
        assert_eq!(store.tokenizer_version().unwrap(), 1);

        store
            .set_tokenizer_version(crate::classify::TOKENIZER_VERSION)
            .unwrap();
        assert_eq!(
            store.tokenizer_version().unwrap(),
            crate::classify::TOKENIZER_VERSION
        );
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
    fn a_database_from_a_newer_build_is_refused() {
        let (dir, store) = open();
        drop(store);
        let path = dir.path().join("state.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 999)
            .unwrap();

        let err = Store::open(&path).unwrap_err();

        assert!(
            matches!(err, Error::NewerSchema { found: 999, .. }),
            "{err}"
        );
    }

    #[test]
    fn only_one_process_may_run_on_a_database() {
        let (dir, _store) = open();
        let path = dir.path().join("state.db");

        let first = lock_instance(&path).unwrap();
        let second = lock_instance(&path).unwrap_err();
        drop(first);

        assert!(matches!(second, Error::AlreadyRunning(_)), "{second}");
        assert!(lock_instance(&path).is_ok());
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
            .record_pending_move(
                &decision("<a>", "bayes", 10),
                "Junk",
                PendingMarks {
                    set: &["$TamisFiled"],
                    ..PendingMarks::default()
                },
            )
            .unwrap();
        let refused = store
            .record_pending_move(
                &decision("<b>", "bayes", 20),
                "Junk",
                PendingMarks {
                    set: &["$TamisFiled"],
                    ..PendingMarks::default()
                },
            )
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
    fn confirming_a_pending_undo_marks_the_decision_it_undoes() {
        let (_dir, store) = open();
        let original = store
            .record_decision(&decision("<a>", "bayes", 10))
            .unwrap();
        let undo = store
            .record_pending_move(
                &decision("<a>", "undo", 20),
                "INBOX",
                PendingMarks {
                    set: &["$TamisTrained"],
                    clear: &["$TamisFiled"],
                    undoes: Some(original),
                },
            )
            .unwrap();

        let pending = store.pending_moves().unwrap();
        assert_eq!(pending[0].keywords, ["$TamisTrained"]);
        assert_eq!(pending[0].restore, ["$TamisFiled"]);
        assert!(!store.decision(original).unwrap().undone);

        store.confirm_move(undo).unwrap();

        assert!(store.decision(original).unwrap().undone);
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

    fn page_through(store: &Store, sort: DecisionSort, asc: bool, limit: i64) -> Vec<i64> {
        let mut ids = Vec::new();
        let mut after = 0;
        loop {
            let (page, _) = store
                .query_decisions(&DecisionQuery {
                    sort,
                    asc,
                    after,
                    limit,
                    ..DecisionQuery::default()
                })
                .unwrap();
            let Some(last) = page.last() else {
                return ids;
            };
            after = last.id;
            ids.extend(page.iter().map(|d| d.id));
        }
    }

    #[test]
    fn keyset_pages_join_up_in_every_sort_order() {
        let (_dir, store) = open();
        for (subject, seconds) in [("b", 10), ("A", 10), ("c", 20), ("a", 10), ("B", 30)] {
            let mut entry = decision("<x>", "bayes", seconds);
            entry.subject = subject.to_owned();
            store.record_decision(&entry).unwrap();
        }

        for sort in [
            DecisionSort::Time,
            DecisionSort::Subject,
            DecisionSort::Verdict,
        ] {
            for asc in [false, true] {
                let (all, _) = store
                    .query_decisions(&DecisionQuery {
                        sort,
                        asc,
                        limit: 100,
                        ..DecisionQuery::default()
                    })
                    .unwrap();
                let want: Vec<i64> = all.iter().map(|d| d.id).collect();

                assert_eq!(
                    page_through(&store, sort, asc, 2),
                    want,
                    "{sort:?} asc={asc}"
                );
            }
        }
    }

    #[test]
    fn address_pages_join_up_in_every_sort_order() {
        let (_dir, store) = open();
        for (address, source) in [
            ("d@x.org", "Sent"),
            ("a@x.org", "sent"),
            ("c@x.org", "train"),
            ("b@x.org", "SENT"),
            ("e@x.org", "train"),
        ] {
            store.add_address(address, source).unwrap();
        }

        for sort in [
            AddressSort::Address,
            AddressSort::Source,
            AddressSort::Added,
        ] {
            for desc in [false, true] {
                let query = |after: String, limit| AddressQuery {
                    sort,
                    desc,
                    after,
                    limit,
                    ..AddressQuery::default()
                };
                let want: Vec<String> = store
                    .query_addresses(&query(String::new(), 100))
                    .unwrap()
                    .0
                    .into_iter()
                    .map(|a| a.address)
                    .collect();

                let mut got: Vec<String> = Vec::new();
                loop {
                    let after = got.last().cloned().unwrap_or_default();
                    let (page, _) = store.query_addresses(&query(after, 2)).unwrap();
                    if page.is_empty() {
                        break;
                    }
                    got.extend(page.into_iter().map(|a| a.address));
                }

                assert_eq!(got, want, "{sort:?} desc={desc}");
            }
        }
    }

    #[test]
    fn a_decision_recorded_between_pages_repeats_and_skips_nothing() {
        let (_dir, store) = open();
        for seconds in [10, 20, 30, 40] {
            store
                .record_decision(&decision("<a>", "bayes", seconds))
                .unwrap();
        }
        let query = |after| DecisionQuery {
            after,
            limit: 2,
            ..DecisionQuery::default()
        };
        let (first, _) = store.query_decisions(&query(0)).unwrap();

        store
            .record_decision(&decision("<a>", "bayes", 50))
            .unwrap();
        let (second, _) = store.query_decisions(&query(first[1].id)).unwrap();

        let ids = |page: &[Decision]| page.iter().map(|d| d.id).collect::<Vec<_>>();
        assert_eq!((ids(&first), ids(&second)), (vec![4, 3], vec![2, 1]));
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
    fn whitelisting_parses_and_records_the_manual_source() {
        let (_dir, store) = open();
        assert_eq!(
            store.whitelist("Alice <Alice@Example.org>").unwrap(),
            "alice@example.org"
        );
        let listed = store.addresses().unwrap();
        assert_eq!(listed[0].source, MANUAL_SOURCE);
        assert!(matches!(
            store.whitelist("nope"),
            Err(Error::InvalidAddress(_))
        ));
    }

    #[test]
    fn listed_addresses_are_parsed_sorted_and_deduplicated() {
        let given = ["B@x.org", "a@x.org", "b@x.org"].map(str::to_owned);
        assert_eq!(parse_addresses(&given).unwrap(), ["a@x.org", "b@x.org"]);
        assert!(matches!(parse_addresses(&[]), Err(Error::NoAddresses)));
        assert!(matches!(
            parse_addresses(&["bad".to_owned()]),
            Err(Error::InvalidAddress(_))
        ));
    }

    #[test]
    fn removing_listed_addresses_refuses_any_not_whitelisted() {
        let (_dir, store) = open();
        store.whitelist("a@x.org").unwrap();
        store.whitelist("b@x.org").unwrap();

        let err = store
            .remove_listed(&["a@x.org".to_owned(), "c@x.org".to_owned()])
            .unwrap_err();
        assert!(matches!(err, Error::NotWhitelisted(a) if a == "c@x.org"));
        assert_eq!(store.addresses().unwrap().len(), 2);

        assert_eq!(store.remove_listed(&["a@x.org".to_owned()]).unwrap(), 1);
        assert_eq!(store.addresses().unwrap().len(), 1);
    }

    #[test]
    fn counting_matches_refuses_an_empty_result() {
        let (_dir, store) = open();
        store.whitelist("a@x.org").unwrap();
        store.whitelist("b@y.org").unwrap();
        assert_eq!(store.count_matching("").unwrap(), 2);
        assert_eq!(store.count_matching("y.org").unwrap(), 1);
        assert!(matches!(store.count_matching("zzz"), Err(Error::NoMatch)));
    }

    #[test]
    fn user_errors_are_told_apart_from_internal_ones() {
        assert!(Error::NoMatch.is_user_error());
        assert!(Error::InvalidAddress(String::new()).is_user_error());
        assert!(!Error::NotFound(String::new()).is_user_error());
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

use jiff::Timestamp;
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Row, params, params_from_iter};

use super::{DEFAULT_QUERY_LIMIT, Error, Result, Store, unix};
use crate::journal::{Action, Reason, Verdict};

/// One journal entry: what tamis concluded about a message and what it did
/// (or, in shadow mode, would have done).
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "mirrors the journal columns")]
pub struct Decision {
    pub id: i64,
    pub at: Timestamp,
    pub mailbox: String,
    pub uid: u32,
    pub message_id: String,
    pub from: String,
    pub subject: String,
    pub score: f64,
    pub verdict: Verdict,
    pub reason: Reason,
    pub action: Action,
    pub applied: bool,
    /// Set once the action has been reversed with an undo.
    pub undone: bool,
    /// The ID of the first later correction or undo of the same message, or 0.
    /// Derived when the decision is read, not stored.
    pub corrected_by: i64,
    /// The ID of the undo entry that reversed this decision, or 0. It differs
    /// from `corrected_by` when a correction came between.
    pub undone_by: i64,
    /// The earlier decisions on the same message that this entry is the first
    /// correction or undo of, oldest first. Derived, and empty for entries
    /// that are not corrections.
    pub corrects: Vec<i64>,
}

/// A move tamis meant to make and may have made: the connection dropped, or
/// the pass was cut off, before it knew.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMove {
    pub decision_id: i64,
    pub source: String,
    pub dest: String,
    pub message_id: String,
    pub keyword: String,
}

/// Summarises what the store has learnt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub spam: i64,
    pub good: i64,
    pub tokens: i64,
    pub addresses: i64,
    /// The journal's size; `oldest_decision` is `None` when it is empty.
    pub decisions: i64,
    pub oldest_decision: Option<Timestamp>,
    /// Tokens seen in a single message: the first to go if the model is pruned.
    pub single_use_tokens: i64,
    /// The database's size, write-ahead log included, and the part of it that is free pages.
    pub bytes: i64,
    pub free_bytes: i64,
}

impl Stats {
    /// One line on the database: its size, what vacuuming would give back, and
    /// what fills it.
    #[must_use]
    pub fn database_line(&self) -> String {
        format!(
            "{} ({} free); {} tokens ({} seen once), {} addresses, {} decisions",
            human_bytes(self.bytes),
            human_bytes(self.free_bytes),
            self.tokens,
            self.single_use_tokens,
            self.addresses,
            self.decisions,
        )
    }
}

#[allow(clippy::cast_precision_loss, reason = "a size for display")]
fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// The columns a decision query can sort by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecisionSort {
    #[default]
    Time,
    Id,
    Verdict,
    Reason,
    Score,
    Action,
    Folder,
    From,
    Subject,
}

impl DecisionSort {
    /// The sort a name stands for; anything unknown sorts by time.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "id" => Self::Id,
            "verdict" => Self::Verdict,
            "reason" => Self::Reason,
            "score" => Self::Score,
            "action" => Self::Action,
            "folder" => Self::Folder,
            "from" => Self::From,
            "subject" => Self::Subject,
            _ => Self::Time,
        }
    }

    /// The SQL ordering term. Only these strings reach the query.
    fn column(self) -> &'static str {
        match self {
            Self::Time => "at",
            Self::Id => "id",
            Self::Verdict => "verdict COLLATE NOCASE",
            Self::Reason => "reason COLLATE NOCASE",
            Self::Score => "score",
            Self::Action => "action COLLATE NOCASE",
            Self::Folder => "mailbox COLLATE NOCASE",
            Self::From => "sender COLLATE NOCASE",
            Self::Subject => "subject COLLATE NOCASE",
        }
    }
}

/// Picks and orders journal entries. The default is the newest decisions
/// first.
#[derive(Clone, Debug, Default)]
pub struct DecisionQuery {
    /// Keeps one decision; zero keeps all.
    pub id: i64,
    /// Keeps one verdict; `None` keeps all.
    pub verdict: Option<Verdict>,
    /// Keeps one reason; `None` keeps all.
    pub reason: Option<Reason>,
    /// Keeps decisions whose sender, subject, reason, action or folder contain
    /// it, ignoring ASCII case.
    pub search: String,
    pub sort: DecisionSort,
    /// Sorts upwards; the default is downwards, newest or highest first.
    pub asc: bool,
    pub offset: i64,
    /// The page size; it defaults to 100.
    pub limit: i64,
}

impl DecisionQuery {
    fn where_clause(&self) -> (String, Vec<Value>) {
        let mut conditions = Vec::new();
        let mut args = Vec::new();

        if self.id > 0 {
            conditions.push("id = ?".to_owned());
            args.push(Value::Integer(self.id));
        }
        if let Some(verdict) = self.verdict {
            conditions.push("verdict = ?".to_owned());
            args.push(Value::Text(verdict.to_string()));
        }
        if let Some(reason) = self.reason {
            conditions.push("reason = ?".to_owned());
            args.push(Value::Text(reason.to_string()));
        }
        if !self.search.is_empty() {
            let pattern = like_pattern(&self.search);
            conditions.push(
                r"(LOWER(sender) LIKE ? ESCAPE '\' OR LOWER(subject) LIKE ? ESCAPE '\'
                OR LOWER(reason) LIKE ? ESCAPE '\' OR LOWER(action) LIKE ? ESCAPE '\'
                OR LOWER(mailbox) LIKE ? ESCAPE '\')"
                    .to_owned(),
            );
            args.extend(std::iter::repeat_n(Value::Text(pattern), 5));
        }

        if conditions.is_empty() {
            (String::new(), args)
        } else {
            (format!(" WHERE {}", conditions.join(" AND ")), args)
        }
    }

    fn order_by(&self) -> String {
        let direction = if self.asc { " ASC" } else { " DESC" };
        let column = self.sort.column();
        // Entries that tie on time are in the order they were recorded; on
        // anything else, the newest comes first.
        let tiebreak = if column == "at" {
            format!(", id{direction}")
        } else {
            ", id DESC".to_owned()
        };
        format!(" ORDER BY {column}{direction}{tiebreak}")
    }
}

/// A `LIKE` pattern that matches `text` literally anywhere, with `\` as the
/// escape character.
pub(super) fn like_pattern(text: &str) -> String {
    let escaped = text
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Also finds the first correction of the same message after the decision.
/// 'correction' and 'undo' are the engine's reasons; the store can't import
/// them. A decision with no Message-ID is never linked.
const DECISION_COLUMNS: &str = "SELECT id, at, mailbox, uid, message_id, sender, subject,
	score, verdict, reason, action, applied, undone,
	(SELECT COALESCE(MIN(c.id), 0) FROM decisions c
	 WHERE decisions.message_id != '' AND c.message_id = decisions.message_id
	   AND c.id > decisions.id AND c.reason IN ('correction', 'undo')),
	(SELECT COALESCE(MIN(u.id), 0) FROM decisions u
	 WHERE decisions.undone = 1 AND decisions.message_id != ''
	   AND u.message_id = decisions.message_id AND u.id > decisions.id AND u.reason = 'undo'),
	(SELECT COALESCE(GROUP_CONCAT(d.id), '') FROM decisions d
	 WHERE decisions.message_id != '' AND d.message_id = decisions.message_id AND d.id < decisions.id
	   AND ((decisions.reason = 'undo' AND d.undone = 1
	         AND decisions.id = (SELECT MIN(u.id) FROM decisions u
	                             WHERE u.message_id = d.message_id AND u.id > d.id AND u.reason = 'undo'))
	     OR (decisions.reason = 'correction'
	         AND decisions.id = (SELECT MIN(c.id) FROM decisions c
	                             WHERE c.message_id = d.message_id AND c.id > d.id
	                               AND c.reason IN ('correction', 'undo')))))";

fn read_decision(row: &Row<'_>) -> rusqlite::Result<Decision> {
    let mut corrects: Vec<i64> = row
        .get::<_, String>(15)?
        .split(',')
        .filter_map(|id| id.parse().ok())
        .collect();
    corrects.sort_unstable();

    Ok(Decision {
        id: row.get(0)?,
        at: unix(row.get(1)?),
        mailbox: row.get(2)?,
        uid: row.get(3)?,
        message_id: row.get(4)?,
        from: row.get(5)?,
        subject: row.get(6)?,
        score: row.get(7)?,
        verdict: word(row, 8)?,
        reason: word(row, 9)?,
        action: word(row, 10)?,
        applied: row.get(11)?,
        undone: row.get(12)?,
        corrected_by: row.get(13)?,
        undone_by: row.get(14)?,
        corrects,
    })
}

/// A column holding one of the journal's words.
fn word<T: std::str::FromStr>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: String = row.get(index)?;
    text.parse().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

impl Store {
    /// Appends to the journal and returns the entry's ID. The ID and derived
    /// fields of `decision` are ignored.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn record_decision(&self, decision: &Decision) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO decisions
             (at, mailbox, uid, message_id, sender, subject, score, verdict, reason, action, applied)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                decision.at.as_second(),
                decision.mailbox,
                decision.uid,
                decision.message_id,
                decision.from,
                decision.subject,
                decision.score,
                decision.verdict.as_str(),
                decision.reason.as_str(),
                decision.action.to_string(),
                decision.applied,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Journals a move that is about to happen, as not yet applied, together
    /// with where to look for the message if the outcome is lost. Returns the
    /// decision's ID.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn record_pending_move(
        &self,
        decision: &Decision,
        dest: &str,
        keyword: &str,
    ) -> Result<i64> {
        self.in_tx(|tx| {
            tx.execute(
                "INSERT INTO decisions
                 (at, mailbox, uid, message_id, sender, subject, score, verdict, reason, action, applied)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
                params![
                    decision.at.as_second(),
                    decision.mailbox,
                    decision.uid,
                    decision.message_id,
                    decision.from,
                    decision.subject,
                    decision.score,
                    decision.verdict.as_str(),
                    decision.reason.as_str(),
                    decision.action.to_string(),
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO pending_moves (decision_id, source, dest, message_id, keyword)
                 VALUES (?, ?, ?, ?, ?)",
                params![id, decision.mailbox, dest, decision.message_id, keyword],
            )?;
            Ok(id)
        })
    }

    /// The moves whose outcome is not known, oldest first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn pending_moves(&self) -> Result<Vec<PendingMove>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT decision_id, source, dest, message_id, keyword
             FROM pending_moves ORDER BY decision_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PendingMove {
                decision_id: row.get(0)?,
                source: row.get(1)?,
                dest: row.get(2)?,
                message_id: row.get(3)?,
                keyword: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks a pending move as made.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn confirm_move(&self, decision_id: i64) -> Result<()> {
        self.in_tx(|tx| {
            tx.execute(
                "UPDATE decisions SET applied = 1 WHERE id = ?",
                [decision_id],
            )?;
            tx.execute(
                "DELETE FROM pending_moves WHERE decision_id = ?",
                [decision_id],
            )?;
            Ok(())
        })
    }

    /// Forgets a pending move that did not happen, and its decision.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn drop_move(&self, decision_id: i64) -> Result<()> {
        self.in_tx(|tx| {
            tx.execute("DELETE FROM decisions WHERE id = ?", [decision_id])?;
            tx.execute(
                "DELETE FROM pending_moves WHERE decision_id = ?",
                [decision_id],
            )?;
            Ok(())
        })
    }

    /// The most recent journal entries, newest first.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn decisions(&self, limit: i64) -> Result<Vec<Decision>> {
        self.read_decisions(
            &format!("{DECISION_COLUMNS} FROM decisions ORDER BY at DESC, id DESC LIMIT ?"),
            &[Value::Integer(limit)],
        )
    }

    /// One page of the journal and how many decisions match in all.
    ///
    /// shortcut: pages are cut by offset, so a decision recorded while someone
    /// is paging can repeat or skip a row; switch to keyset paging if that
    /// matters.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn query_decisions(&self, query: &DecisionQuery) -> Result<(Vec<Decision>, i64)> {
        let (where_clause, mut args) = query.where_clause();
        let total = self.conn().query_row(
            &format!("SELECT COUNT(*) FROM decisions{where_clause}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        let limit = if query.limit <= 0 {
            DEFAULT_QUERY_LIMIT
        } else {
            query.limit
        };
        args.push(Value::Integer(limit));
        args.push(Value::Integer(query.offset.max(0)));

        let decisions = self.read_decisions(
            &format!(
                "{DECISION_COLUMNS} FROM decisions{where_clause}{} LIMIT ? OFFSET ?",
                query.order_by()
            ),
            &args,
        )?;
        Ok((decisions, total))
    }

    /// Deletes journal entries older than `before` and returns how many it
    /// removed.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn prune_decisions(&self, before: Timestamp) -> Result<usize> {
        Ok(self
            .conn()
            .execute("DELETE FROM decisions WHERE at < ?", [before.as_second()])?)
    }

    /// One journal entry.
    ///
    /// # Errors
    /// [`Error::NotFound`] when there is no such entry.
    pub fn decision(&self, id: i64) -> Result<Decision> {
        self.conn()
            .query_row(
                &format!("{DECISION_COLUMNS} FROM decisions WHERE id = ?"),
                [id],
                read_decision,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("decision {id}")))
    }

    /// The most recent journal entry for a Message-ID. An empty ID matches
    /// nothing.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn latest_decision(&self, message_id: &str) -> Result<Option<Decision>> {
        if message_id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .conn()
            .query_row(
                &format!("{DECISION_COLUMNS} FROM decisions WHERE message_id = ? ORDER BY id DESC LIMIT 1"),
                [message_id],
                read_decision,
            )
            .optional()?)
    }

    /// Records that a decision's action was reversed.
    ///
    /// # Errors
    /// Fails if the decision doesn't exist ([`Error::NotFound`]) or was
    /// already undone ([`Error::AlreadyUndone`]).
    pub fn mark_undone(&self, id: i64) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE decisions SET undone = 1 WHERE id = ? AND undone = 0",
            [id],
        )?;
        if changed > 0 {
            return Ok(());
        }
        self.decision(id)?;
        Err(Error::AlreadyUndone(id))
    }

    /// Counts of trained messages, distinct tokens, whitelisted addresses and
    /// journal entries.
    ///
    /// # Errors
    /// Fails on a database error.
    pub fn stats(&self) -> Result<Stats> {
        let conn = self.conn();
        let wal = conn
            .path()
            .and_then(|path| std::fs::metadata(format!("{path}-wal")).ok())
            .map_or(0, |meta| i64::try_from(meta.len()).unwrap_or(i64::MAX));
        let mut stats: Stats = conn.query_row(
            "SELECT
             (SELECT COALESCE(SUM(label = ?), 0) FROM trained),
             (SELECT COALESCE(SUM(label = ?), 0) FROM trained),
             (SELECT COUNT(*) FROM tokens),
             (SELECT COUNT(*) FROM addresses),
             (SELECT COUNT(*) FROM decisions),
             (SELECT MIN(at) FROM decisions),
             (SELECT COUNT(*) FROM tokens WHERE spam + good = 1),
             (SELECT page_count * page_size FROM pragma_page_count, pragma_page_size),
             (SELECT freelist_count * page_size FROM pragma_freelist_count, pragma_page_size)",
            params![1, 0],
            |row| {
                Ok(Stats {
                    spam: row.get(0)?,
                    good: row.get(1)?,
                    tokens: row.get(2)?,
                    addresses: row.get(3)?,
                    decisions: row.get(4)?,
                    oldest_decision: row.get::<_, Option<i64>>(5)?.map(unix),
                    single_use_tokens: row.get(6)?,
                    bytes: row.get(7)?,
                    free_bytes: row.get(8)?,
                })
            },
        )?;
        stats.bytes += wal;
        Ok(stats)
    }

    fn read_decisions(&self, sql: &str, args: &[Value]) -> Result<Vec<Decision>> {
        let conn = self.conn();
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map(params_from_iter(args.iter()), read_decision)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

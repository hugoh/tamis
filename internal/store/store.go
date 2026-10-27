// Package store keeps tamis's state in SQLite: token statistics, the
// whitelist, per-folder sync cursors and the decision journal.
package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	// Registers the pure-Go "sqlite" driver.
	_ "modernc.org/sqlite"
)

const schemaVersion = 1

const schema = `
CREATE TABLE tokens (
	token TEXT PRIMARY KEY,
	spam  INTEGER NOT NULL DEFAULT 0,
	good  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE trained (
	key    TEXT PRIMARY KEY,
	label  INTEGER NOT NULL,
	tokens TEXT NOT NULL
);
CREATE TABLE addresses (
	address  TEXT PRIMARY KEY,
	source   TEXT NOT NULL,
	added_at INTEGER NOT NULL
);
CREATE TABLE cursors (
	mailbox      TEXT PRIMARY KEY,
	uid_validity INTEGER NOT NULL,
	last_uid     INTEGER NOT NULL
);
CREATE TABLE decisions (
	id         INTEGER PRIMARY KEY,
	at         INTEGER NOT NULL,
	mailbox    TEXT NOT NULL,
	uid        INTEGER NOT NULL,
	message_id TEXT NOT NULL,
	sender     TEXT NOT NULL,
	subject    TEXT NOT NULL,
	score      REAL NOT NULL,
	verdict    TEXT NOT NULL,
	reason     TEXT NOT NULL,
	action     TEXT NOT NULL,
	applied    INTEGER NOT NULL
);
`

// Store is an open state database.
type Store struct {
	db *sql.DB
}

// Open opens or creates the database at path.
func Open(ctx context.Context, path string) (*Store, error) {
	// Immediate transactions take the write lock up front, so two processes
	// migrating a new database can't both decide the schema is missing.
	dsn := "file:" + path + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_txlock=immediate"

	conn, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}

	// SQLite allows one writer; a single connection avoids busy errors.
	conn.SetMaxOpenConns(1)

	opened := &Store{db: conn}
	if err := opened.migrateWithRetry(ctx); err != nil {
		_ = conn.Close()

		return nil, err
	}

	return opened, nil
}

// Close closes the database.
func (s *Store) Close() error {
	if err := s.db.Close(); err != nil {
		return fmt.Errorf("close store: %w", err)
	}

	return nil
}

// Train records the message identified by key as label. Training a message
// again with the same label does nothing; with the other label it reverses
// the earlier training first. It reports whether anything changed.
func (s *Store) Train(
	ctx context.Context,
	key string,
	tokens []string,
	label classify.Label,
) (bool, error) {
	changed := false

	err := s.inTx(ctx, func(txn *sql.Tx) error {
		var (
			oldLabel  classify.Label
			oldTokens string
		)

		err := txn.QueryRowContext(ctx, "SELECT label, tokens FROM trained WHERE key = ?", key).
			Scan(&oldLabel, &oldTokens)

		switch {
		case errors.Is(err, sql.ErrNoRows):
		case err != nil:
			return fmt.Errorf("look up %s: %w", key, err)
		case oldLabel == label:
			return nil
		default:
			var previous []string
			if err := json.Unmarshal([]byte(oldTokens), &previous); err != nil {
				return fmt.Errorf("decode tokens of %s: %w", key, err)
			}

			if err := adjust(ctx, txn, previous, oldLabel, -1); err != nil {
				return err
			}
		}

		if err := adjust(ctx, txn, tokens, label, 1); err != nil {
			return err
		}

		encoded, err := json.Marshal(tokens)
		if err != nil {
			return fmt.Errorf("encode tokens: %w", err)
		}

		_, err = txn.ExecContext(ctx,
			`INSERT INTO trained (key, label, tokens) VALUES (?, ?, ?)
			 ON CONFLICT(key) DO UPDATE SET label = excluded.label, tokens = excluded.tokens`,
			key, label, string(encoded))
		if err != nil {
			return fmt.Errorf("record training of %s: %w", key, err)
		}

		changed = true

		return nil
	})

	return changed, err
}

// Trained returns the label a message was trained with, if it was.
func (s *Store) Trained(ctx context.Context, key string) (classify.Label, bool, error) {
	var label classify.Label

	err := s.db.QueryRowContext(ctx, "SELECT label FROM trained WHERE key = ?", key).Scan(&label)
	if errors.Is(err, sql.ErrNoRows) {
		return classify.Good, false, nil
	}

	if err != nil {
		return classify.Good, false, fmt.Errorf("look up %s: %w", key, err)
	}

	return label, true, nil
}

// Model returns the statistics of the given tokens, for scoring.
func (s *Store) Model(ctx context.Context, tokens []string) (*Snapshot, error) {
	model := &Snapshot{counts: make(map[string][2]int, len(tokens))}

	err := s.db.QueryRowContext(ctx,
		"SELECT COALESCE(SUM(label = ?), 0), COALESCE(SUM(label = ?), 0) FROM trained",
		classify.Spam, classify.Good).Scan(&model.totalSpam, &model.totalGood)
	if err != nil {
		return nil, fmt.Errorf("count trained messages: %w", err)
	}

	if err := model.load(ctx, s.db, tokens); err != nil {
		return nil, err
	}

	return model, nil
}

// AddAddress adds an address to the whitelist. Addresses compare
// case-insensitively.
func (s *Store) AddAddress(ctx context.Context, address, source string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO addresses (address, source, added_at) VALUES (?, ?, ?)
		 ON CONFLICT(address) DO NOTHING`,
		strings.ToLower(address), source, time.Now().Unix())
	if err != nil {
		return fmt.Errorf("whitelist %s: %w", address, err)
	}

	return nil
}

// HasAddress reports whether address is whitelisted.
func (s *Store) HasAddress(ctx context.Context, address string) (bool, error) {
	var found int

	err := s.db.QueryRowContext(ctx,
		"SELECT COUNT(*) FROM addresses WHERE address = ?", strings.ToLower(address)).Scan(&found)
	if err != nil {
		return false, fmt.Errorf("look up %s: %w", address, err)
	}

	return found > 0, nil
}

// Cursor is how far a folder has been processed: the highest UID seen,
// valid only while the folder's UIDVALIDITY is unchanged.
type Cursor struct {
	UIDValidity uint32
	LastUID     uint32
}

// Cursor returns the folder's cursor, if one was saved.
func (s *Store) Cursor(ctx context.Context, mailbox string) (Cursor, bool, error) {
	var cursor Cursor

	err := s.db.QueryRowContext(ctx,
		"SELECT uid_validity, last_uid FROM cursors WHERE mailbox = ?", mailbox).
		Scan(&cursor.UIDValidity, &cursor.LastUID)
	if errors.Is(err, sql.ErrNoRows) {
		return Cursor{}, false, nil
	}

	if err != nil {
		return Cursor{}, false, fmt.Errorf("read cursor for %s: %w", mailbox, err)
	}

	return cursor, true, nil
}

// SetCursor saves the folder's cursor.
func (s *Store) SetCursor(ctx context.Context, mailbox string, cursor Cursor) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO cursors (mailbox, uid_validity, last_uid) VALUES (?, ?, ?)
		 ON CONFLICT(mailbox) DO UPDATE SET
		 uid_validity = excluded.uid_validity, last_uid = excluded.last_uid`,
		mailbox, cursor.UIDValidity, cursor.LastUID)
	if err != nil {
		return fmt.Errorf("save cursor for %s: %w", mailbox, err)
	}

	return nil
}

// Decision is one journal entry: what tamis concluded about a message and
// what it did (or, in shadow mode, would have done).
type Decision struct {
	ID        int64
	At        time.Time
	Mailbox   string
	UID       uint32
	MessageID string
	From      string
	Subject   string
	Score     float64
	Verdict   string
	Reason    string
	Action    string
	Applied   bool
}

// RecordDecision appends to the journal and returns the entry's ID.
func (s *Store) RecordDecision(ctx context.Context, decision Decision) (int64, error) {
	result, err := s.db.ExecContext(ctx,
		`INSERT INTO decisions
		 (at, mailbox, uid, message_id, sender, subject, score, verdict, reason, action, applied)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		decision.At.Unix(), decision.Mailbox, decision.UID, decision.MessageID, decision.From,
		decision.Subject, decision.Score, decision.Verdict, decision.Reason, decision.Action,
		decision.Applied)
	if err != nil {
		return 0, fmt.Errorf("record decision: %w", err)
	}

	id, err := result.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("record decision: %w", err)
	}

	return id, nil
}

// Decisions returns the most recent journal entries, newest first.
func (s *Store) Decisions(ctx context.Context, limit int) ([]Decision, error) {
	rows, err := s.db.QueryContext(
		ctx,
		`SELECT id, at, mailbox, uid, message_id, sender, subject, score, verdict, reason, action, applied
		 FROM decisions ORDER BY at DESC, id DESC LIMIT ?`,
		limit,
	)
	if err != nil {
		return nil, fmt.Errorf("list decisions: %w", err)
	}

	defer func() { _ = rows.Close() }()

	var decisions []Decision

	for rows.Next() {
		var (
			decision Decision
			unixAt   int64
		)

		err := rows.Scan(&decision.ID, &unixAt, &decision.Mailbox, &decision.UID,
			&decision.MessageID, &decision.From, &decision.Subject, &decision.Score,
			&decision.Verdict, &decision.Reason, &decision.Action, &decision.Applied)
		if err != nil {
			return nil, fmt.Errorf("scan decision: %w", err)
		}

		decision.At = time.Unix(unixAt, 0).UTC()
		decisions = append(decisions, decision)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list decisions: %w", err)
	}

	return decisions, nil
}

// Stats summarises what the store has learnt.
type Stats struct {
	Spam      int
	Good      int
	Tokens    int
	Addresses int
}

// Stats returns counts of trained messages, distinct tokens and whitelisted
// addresses.
func (s *Store) Stats(ctx context.Context) (Stats, error) {
	var stats Stats

	err := s.db.QueryRowContext(ctx, `SELECT
		(SELECT COALESCE(SUM(label = ?), 0) FROM trained),
		(SELECT COALESCE(SUM(label = ?), 0) FROM trained),
		(SELECT COUNT(*) FROM tokens),
		(SELECT COUNT(*) FROM addresses)`, classify.Spam, classify.Good).
		Scan(&stats.Spam, &stats.Good, &stats.Tokens, &stats.Addresses)
	if err != nil {
		return Stats{}, fmt.Errorf("read stats: %w", err)
	}

	return stats, nil
}

// migrateWithRetry retries while another process creates the database:
// SQLite reports SQLITE_BUSY at once, without waiting, while a new file is
// switched to WAL mode.
func (s *Store) migrateWithRetry(ctx context.Context) error {
	const (
		attempts = 20
		backoff  = 50 * time.Millisecond
	)

	var err error

	for range attempts {
		if err = s.migrate(ctx); !isBusy(err) {
			return err
		}

		select {
		case <-ctx.Done():
			return errors.Join(err, ctx.Err())
		case <-time.After(backoff):
		}
	}

	return err
}

func isBusy(err error) bool {
	const sqliteBusy = 5

	coded, ok := errors.AsType[interface {
		error
		Code() int
	}](err)

	return ok && coded.Code()&0xff == sqliteBusy
}

func (s *Store) migrate(ctx context.Context) error {
	return s.inTx(ctx, func(txn *sql.Tx) error {
		var version int
		if err := txn.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
			return fmt.Errorf("read schema version: %w", err)
		}

		if version >= schemaVersion {
			return nil
		}

		if _, err := txn.ExecContext(ctx, schema); err != nil {
			return fmt.Errorf("create schema: %w", err)
		}

		pragma := fmt.Sprintf("PRAGMA user_version = %d", schemaVersion)
		if _, err := txn.ExecContext(ctx, pragma); err != nil {
			return fmt.Errorf("set schema version: %w", err)
		}

		return nil
	})
}

func (s *Store) inTx(ctx context.Context, body func(*sql.Tx) error) error {
	txn, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin: %w", err)
	}

	if err := body(txn); err != nil {
		return errors.Join(err, txn.Rollback())
	}

	if err := txn.Commit(); err != nil {
		return fmt.Errorf("commit: %w", err)
	}

	return nil
}

func adjust(
	ctx context.Context,
	txn *sql.Tx,
	tokens []string,
	label classify.Label,
	delta int,
) error {
	spam, good := 0, 0
	if label == classify.Spam {
		spam = delta
	} else {
		good = delta
	}

	encoded, err := json.Marshal(tokens)
	if err != nil {
		return fmt.Errorf("encode tokens: %w", err)
	}

	_, err = txn.ExecContext(ctx,
		`INSERT INTO tokens (token, spam, good)
		 SELECT value, ?, ? FROM json_each(?) WHERE true
		 ON CONFLICT(token) DO UPDATE SET
		 spam = spam + excluded.spam, good = good + excluded.good`,
		spam, good, string(encoded))
	if err != nil {
		return fmt.Errorf("update tokens: %w", err)
	}

	if delta < 0 {
		if _, err := txn.ExecContext(
			ctx,
			"DELETE FROM tokens WHERE spam <= 0 AND good <= 0",
		); err != nil {
			return fmt.Errorf("prune tokens: %w", err)
		}
	}

	return nil
}

// Snapshot is a classify.Model loaded from the store for a set of tokens.
type Snapshot struct {
	counts               map[string][2]int
	totalSpam, totalGood int
}

// Counts implements classify.Model.
func (m *Snapshot) Counts(token string) (int, int) {
	c := m.counts[token]

	return c[classify.Spam], c[classify.Good]
}

// Totals implements classify.Model.
func (m *Snapshot) Totals() (int, int) {
	return m.totalSpam, m.totalGood
}

func (m *Snapshot) load(ctx context.Context, conn *sql.DB, tokens []string) error {
	encoded, err := json.Marshal(tokens)
	if err != nil {
		return fmt.Errorf("encode tokens: %w", err)
	}

	rows, err := conn.QueryContext(ctx,
		"SELECT token, spam, good FROM tokens WHERE token IN (SELECT value FROM json_each(?))",
		string(encoded))
	if err != nil {
		return fmt.Errorf("load tokens: %w", err)
	}

	defer func() { _ = rows.Close() }()

	for rows.Next() {
		var (
			token      string
			spam, good int
		)

		if err := rows.Scan(&token, &spam, &good); err != nil {
			return fmt.Errorf("scan token: %w", err)
		}

		var counts [2]int

		counts[classify.Spam], counts[classify.Good] = spam, good
		m.counts[token] = counts
	}

	if err := rows.Err(); err != nil {
		return fmt.Errorf("load tokens: %w", err)
	}

	return nil
}

package store_test

import (
	"database/sql"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

func open(t *testing.T) *store.Store {
	t.Helper()

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, db.Close()) })

	return db
}

func counts(t *testing.T, db *store.Store, token string) (int, int, int, int) {
	t.Helper()

	model, err := db.Model(t.Context(), []string{token})
	require.NoError(t, err)

	spam, good := model.Counts(token)
	totalSpam, totalGood := model.Totals()

	return spam, good, totalSpam, totalGood
}

func TestOpenTwiceKeepsData(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "tamis.db")

	db, err := store.Open(t.Context(), path)
	require.NoError(t, err)
	require.NoError(t, db.AddAddress(t.Context(), "a@example.com", "test"))
	require.NoError(t, db.Close())

	db, err = store.Open(t.Context(), path)
	require.NoError(t, err)

	defer func() { require.NoError(t, db.Close()) }()

	ok, err := db.HasAddress(t.Context(), "a@example.com")
	require.NoError(t, err)
	require.True(t, ok)
}

func TestTrain(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name                         string
		steps                        []classify.Label
		spam, good, totSpam, totGood int
		changed                      []bool
	}{
		{"once", []classify.Label{classify.Spam}, 1, 0, 1, 0, []bool{true}},
		{
			"same label twice",
			[]classify.Label{classify.Spam, classify.Spam},
			1,
			0,
			1,
			0,
			[]bool{true, false},
		},
		{
			"relabelled",
			[]classify.Label{classify.Spam, classify.Good},
			0,
			1,
			0,
			1,
			[]bool{true, true},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			db := open(t)

			for i, label := range tt.steps {
				changed, err := db.Train(t.Context(), "<msg@example>", []string{"token"}, label)
				require.NoError(t, err)
				require.Equal(t, tt.changed[i], changed, "step %d", i)
			}

			spam, good, totSpam, totGood := counts(t, db, "token")
			require.Equal(
				t,
				[]int{tt.spam, tt.good, tt.totSpam, tt.totGood},
				[]int{spam, good, totSpam, totGood},
			)
		})
	}
}

func TestModelOnlyLoadsRequestedTokens(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, err := db.Train(t.Context(), "m1", []string{"alpha", "beta"}, classify.Good)
	require.NoError(t, err)

	model, err := db.Model(t.Context(), []string{"alpha", "missing"})
	require.NoError(t, err)

	_, good := model.Counts("alpha")
	require.Equal(t, 1, good)

	spam, good := model.Counts("missing")
	require.Zero(t, spam+good)
}

func TestWhitelist(t *testing.T) {
	t.Parallel()

	db := open(t)

	require.NoError(t, db.AddAddress(t.Context(), "Friend@Example.COM", "sent"))
	require.NoError(t, db.AddAddress(t.Context(), "friend@example.com", "sent"))

	for address, want := range map[string]bool{
		"friend@example.com":   true,
		"FRIEND@example.com":   true,
		"stranger@example.com": false,
	} {
		ok, err := db.HasAddress(t.Context(), address)
		require.NoError(t, err)
		require.Equal(t, want, ok, address)
	}

	stats, err := db.Stats(t.Context())
	require.NoError(t, err)
	require.Equal(t, 1, stats.Addresses)
}

func TestCursor(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, ok, err := db.Cursor(t.Context(), "INBOX")
	require.NoError(t, err)
	require.False(t, ok)

	require.NoError(
		t,
		db.SetCursor(t.Context(), "INBOX", store.Cursor{UIDValidity: 7, LastUID: 42}),
	)
	require.NoError(
		t,
		db.SetCursor(t.Context(), "INBOX", store.Cursor{UIDValidity: 7, LastUID: 43}),
	)

	cursor, ok, err := db.Cursor(t.Context(), "INBOX")
	require.NoError(t, err)
	require.True(t, ok)
	require.Equal(t, store.Cursor{UIDValidity: 7, LastUID: 43}, cursor)
}

func TestDecisions(t *testing.T) {
	t.Parallel()

	db := open(t)

	at := time.Date(2026, 10, 27, 9, 0, 0, 0, time.UTC)

	for i, verdict := range []string{"good", verdictSpam} {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At:        at.Add(time.Duration(i) * time.Minute),
			Mailbox:   "INBOX",
			UID:       uint32(i + 1),
			MessageID: "<m" + verdict + ">",
			From:      "x@example.com",
			Subject:   verdict + " subject",
			Score:     0.5,
			Verdict:   verdict,
			Reason:    "bayes",
			Action:    "none",
			Applied:   false,
		})
		require.NoError(t, err)
	}

	decisions, err := db.Decisions(t.Context(), 10)
	require.NoError(t, err)
	require.Len(t, decisions, 2)
	require.Equal(t, verdictSpam, decisions[0].Verdict, "newest first")
	require.Equal(t, at.Add(time.Minute), decisions[0].At)
	require.Equal(t, "bayes", decisions[0].Reason)
	require.Positive(t, decisions[0].ID)
}

func TestStats(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, err := db.Train(t.Context(), "m1", []string{"a", "b"}, classify.Spam)
	require.NoError(t, err)
	_, err = db.Train(t.Context(), "m2", []string{"a"}, classify.Good)
	require.NoError(t, err)

	stats, err := db.Stats(t.Context())
	require.NoError(t, err)
	require.Equal(t, store.Stats{Spam: 1, Good: 1, Tokens: 2}, stats)
}

func TestClosedStoreFails(t *testing.T) {
	t.Parallel()

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	require.NoError(t, db.Close())

	tests := map[string]func(t *testing.T){
		"train": func(t *testing.T) {
			t.Helper()

			_, err := db.Train(t.Context(), "k", []string{"t"}, classify.Spam)
			require.Error(t, err)
		},
		"model": func(t *testing.T) {
			t.Helper()

			_, err := db.Model(t.Context(), []string{"t"})
			require.Error(t, err)
		},
		"add address": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.AddAddress(t.Context(), "a@b", "x"))
		},
		"has address": func(t *testing.T) {
			t.Helper()

			_, err := db.HasAddress(t.Context(), "a@b")
			require.Error(t, err)
		},
		"cursor": func(t *testing.T) {
			t.Helper()

			_, _, err := db.Cursor(t.Context(), "INBOX")
			require.Error(t, err)
		},
		"set cursor": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.SetCursor(t.Context(), "INBOX", store.Cursor{}))
		},
		"record decision": func(t *testing.T) {
			t.Helper()

			_, err := db.RecordDecision(t.Context(), store.Decision{})
			require.Error(t, err)
		},
		"decisions": func(t *testing.T) {
			t.Helper()

			_, err := db.Decisions(t.Context(), 1)
			require.Error(t, err)
		},
		"trained": func(t *testing.T) {
			t.Helper()

			_, _, err := db.Trained(t.Context(), "k")
			require.Error(t, err)
		},
		"decision": func(t *testing.T) {
			t.Helper()

			_, err := db.Decision(t.Context(), 1)
			require.Error(t, err)
		},
		"mark undone": func(t *testing.T) {
			t.Helper()
			require.Error(t, db.MarkUndone(t.Context(), 1))
		},
		"latest decision": func(t *testing.T) {
			t.Helper()

			_, _, err := db.LatestDecision(t.Context(), "<m@x>")
			require.Error(t, err)
		},
		"remove address": func(t *testing.T) {
			t.Helper()
			require.Error(t, db.RemoveAddress(t.Context(), "a@b"))
		},
		"stats": func(t *testing.T) {
			t.Helper()

			_, err := db.Stats(t.Context())
			require.Error(t, err)
		},
	}

	for name, check := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			check(t)
		})
	}
}

func TestOpenBadPath(t *testing.T) {
	t.Parallel()

	_, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "missing", "dir", "tamis.db"))
	require.Error(t, err)
}

func TestTrained(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, ok, err := db.Trained(t.Context(), "m1")
	require.NoError(t, err)
	require.False(t, ok)

	_, err = db.Train(t.Context(), "m1", []string{"x"}, classify.Spam)
	require.NoError(t, err)

	label, ok, err := db.Trained(t.Context(), "m1")
	require.NoError(t, err)
	require.True(t, ok)
	require.Equal(t, classify.Spam, label)
}

func TestConcurrentOpenOfNewDatabase(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "tamis.db")

	const openers = 8

	errs := make(chan error, openers)

	for range openers {
		go func() {
			db, err := store.Open(t.Context(), path)
			if err == nil {
				err = db.Close()
			}

			errs <- err
		}()
	}

	for range openers {
		require.NoError(t, <-errs)
	}
}

const verdictSpam = "spam"

// v1Schema is the decisions table as released in schema version 1.
const v1Schema = `
CREATE TABLE decisions (
	id INTEGER PRIMARY KEY, at INTEGER NOT NULL, mailbox TEXT NOT NULL,
	uid INTEGER NOT NULL, message_id TEXT NOT NULL, sender TEXT NOT NULL,
	subject TEXT NOT NULL, score REAL NOT NULL, verdict TEXT NOT NULL,
	reason TEXT NOT NULL, action TEXT NOT NULL, applied INTEGER NOT NULL
);
INSERT INTO decisions VALUES (1, 0, 'INBOX', 7, '<m@x>', 'a@x', 's', 0.95, 'spam', 'bayes', 'move:Junk', 1);
PRAGMA user_version = 1;
`

func TestMigrateFromV1(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "tamis.db")

	raw, err := sql.Open("sqlite", path)
	require.NoError(t, err)
	_, err = raw.ExecContext(t.Context(), v1Schema)
	require.NoError(t, err)
	require.NoError(t, raw.Close())

	db, err := store.Open(t.Context(), path)
	require.NoError(t, err)

	defer func() { require.NoError(t, db.Close()) }()

	decision, err := db.Decision(t.Context(), 1)
	require.NoError(t, err)
	require.Equal(t, "<m@x>", decision.MessageID)
	require.False(t, decision.Undone)

	require.NoError(t, db.MarkUndone(t.Context(), 1))
}

func TestDecisionLookup(t *testing.T) {
	t.Parallel()

	db := open(t)

	id, err := db.RecordDecision(t.Context(), store.Decision{
		At: time.Unix(0, 0).
			UTC(),
		Mailbox:   "INBOX",
		UID:       3,
		MessageID: "<a@b>",
		Verdict:   verdictSpam,
		Action:    "move:Junk",
		Applied:   true,
	})
	require.NoError(t, err)

	decision, err := db.Decision(t.Context(), id)
	require.NoError(t, err)
	require.Equal(t, id, decision.ID)
	require.Equal(t, "<a@b>", decision.MessageID)

	_, err = db.Decision(t.Context(), id+100)
	require.ErrorIs(t, err, store.ErrNotFound)
}

func TestMarkUndone(t *testing.T) {
	t.Parallel()

	db := open(t)

	id, err := db.RecordDecision(t.Context(), store.Decision{Action: "move:Junk", Applied: true})
	require.NoError(t, err)

	require.NoError(t, db.MarkUndone(t.Context(), id))
	require.ErrorIs(t, db.MarkUndone(t.Context(), id), store.ErrAlreadyUndone)
	require.ErrorIs(t, db.MarkUndone(t.Context(), id+100), store.ErrNotFound)

	decisions, err := db.Decisions(t.Context(), 10)
	require.NoError(t, err)
	require.True(t, decisions[0].Undone)
}

func TestLatestDecision(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, ok, err := db.LatestDecision(t.Context(), "<m@x>")
	require.NoError(t, err)
	require.False(t, ok)

	for _, verdict := range []string{verdictSpam, "good"} {
		_, err := db.RecordDecision(
			t.Context(),
			store.Decision{MessageID: "<m@x>", Verdict: verdict},
		)
		require.NoError(t, err)
	}

	_, err = db.RecordDecision(
		t.Context(),
		store.Decision{MessageID: "<other@x>", Verdict: verdictSpam},
	)
	require.NoError(t, err)

	decision, ok, err := db.LatestDecision(t.Context(), "<m@x>")
	require.NoError(t, err)
	require.True(t, ok)
	require.Equal(t, "good", decision.Verdict)

	_, ok, err = db.LatestDecision(t.Context(), "")
	require.NoError(t, err)
	require.False(t, ok, "an empty Message-ID matches nothing")
}

func TestRemoveAddress(t *testing.T) {
	t.Parallel()

	db := open(t)

	require.NoError(t, db.AddAddress(t.Context(), "friend@example.com", "sent"))
	require.NoError(t, db.RemoveAddress(t.Context(), "Friend@Example.com"))
	require.NoError(t, db.RemoveAddress(t.Context(), "nobody@example.com"))

	ok, err := db.HasAddress(t.Context(), "friend@example.com")
	require.NoError(t, err)
	require.False(t, ok)
}

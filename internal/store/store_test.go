package store_test

import (
	"database/sql"
	"os"
	"path/filepath"
	"runtime"
	"strings"
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

	_, ok, err := db.Cursor(t.Context(), inbox)
	require.NoError(t, err)
	require.False(t, ok)

	require.NoError(
		t,
		db.SetCursor(t.Context(), inbox, store.Cursor{UIDValidity: 7, LastUID: 42}),
	)
	require.NoError(
		t,
		db.SetCursor(t.Context(), inbox, store.Cursor{UIDValidity: 7, LastUID: 43}),
	)

	cursor, ok, err := db.Cursor(t.Context(), inbox)
	require.NoError(t, err)
	require.True(t, ok)
	require.Equal(t, store.Cursor{UIDValidity: 7, LastUID: 43}, cursor)
}

func TestModSeq(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, ok, err := db.ModSeq(t.Context(), inbox)
	require.NoError(t, err)
	require.False(t, ok)

	require.NoError(
		t,
		db.SetModSeq(t.Context(), inbox, store.ModSeq{UIDValidity: 7, Highest: 1 << 40}),
	)
	require.NoError(
		t,
		db.SetModSeq(t.Context(), inbox, store.ModSeq{UIDValidity: 7, Highest: 1<<40 + 1}),
	)

	modSeq, ok, err := db.ModSeq(t.Context(), inbox)
	require.NoError(t, err)
	require.True(t, ok)
	require.Equal(t, store.ModSeq{UIDValidity: 7, Highest: 1<<40 + 1}, modSeq)
}

func TestDecisions(t *testing.T) {
	t.Parallel()

	db := open(t)

	at := time.Date(2026, 10, 27, 9, 0, 0, 0, time.UTC)

	for i, verdict := range []string{verdictGood, verdictSpam} {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At:        at.Add(time.Duration(i) * time.Minute),
			Mailbox:   inbox,
			UID:       uint32(i + 1),
			MessageID: "<m" + verdict + ">",
			From:      "x@example.com",
			Subject:   verdict + " subject",
			Score:     0.5,
			Verdict:   verdict,
			Reason:    reasonBayes,
			Action:    actionNone,
			Applied:   false,
		})
		require.NoError(t, err)
	}

	decisions, err := db.Decisions(t.Context(), 10)
	require.NoError(t, err)
	require.Len(t, decisions, 2)
	require.Equal(t, verdictSpam, decisions[0].Verdict, "newest first")
	require.Equal(t, at.Add(time.Minute), decisions[0].At)
	require.Equal(t, reasonBayes, decisions[0].Reason)
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

	oldest := time.Date(2026, 10, 27, 9, 0, 0, 0, time.UTC)

	for _, at := range []time.Time{oldest.Add(time.Hour), oldest} {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At: at, Mailbox: inbox, MessageID: "<m>", Verdict: verdictGood,
			Reason: reasonBayes, Action: actionNone,
		})
		require.NoError(t, err)
	}

	stats, err = db.Stats(t.Context())
	require.NoError(t, err)
	require.Equal(t, 2, stats.Decisions)
	require.Equal(t, oldest, stats.OldestDecision)
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

			_, _, err := db.Cursor(t.Context(), inbox)
			require.Error(t, err)
		},
		"set cursor": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.SetCursor(t.Context(), inbox, store.Cursor{}))
		},
		"mod seq": func(t *testing.T) {
			t.Helper()

			_, _, err := db.ModSeq(t.Context(), inbox)
			require.Error(t, err)
		},
		"set mod seq": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.SetModSeq(t.Context(), inbox, store.ModSeq{}))
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
		"prune decisions": func(t *testing.T) {
			t.Helper()

			_, err := db.PruneDecisions(t.Context(), time.Now())
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

const (
	verdictSpam = "spam"
	verdictGood = "good"
	inbox       = "INBOX"
	reasonBayes = "bayes"
	actionNone  = "none"
)

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
		Mailbox:   inbox,
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

	for _, verdict := range []string{verdictSpam, verdictGood} {
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
	require.Equal(t, verdictGood, decision.Verdict)

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

func TestDatabaseIsPrivate(t *testing.T) {
	t.Parallel()

	if runtime.GOOS == "windows" {
		t.Skip("POSIX permissions")
	}

	tests := map[string]bool{"new database": false, "existing readable database": true}

	for name, exists := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			path := filepath.Join(t.TempDir(), "tamis.db")

			if exists {
				require.NoError(t, os.WriteFile(path, nil, 0o600))
				require.NoError(t, os.Chmod(path, 0o644)) //nolint:gosec // the state under test
			}

			db, err := store.Open(t.Context(), path)
			require.NoError(t, err)
			require.NoError(t, db.AddAddress(t.Context(), "a@example.com", "test"))

			for _, file := range []string{path, path + "-wal"} {
				info, err := os.Stat(file)
				require.NoError(t, err)
				require.Equal(t, os.FileMode(0o600), info.Mode().Perm(), file)
			}

			require.NoError(t, db.Close())
		})
	}
}

func TestPruneDecisions(t *testing.T) {
	t.Parallel()

	db := open(t)
	now := time.Date(2026, 10, 29, 12, 0, 0, 0, time.UTC)

	for _, age := range []time.Duration{400 * 24 * time.Hour, 366 * 24 * time.Hour, time.Hour} {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At: now.Add(-age), Mailbox: inbox, MessageID: "<m>", Verdict: verdictGood,
			Reason: reasonBayes, Action: actionNone,
		})
		require.NoError(t, err)
	}

	pruned, err := db.PruneDecisions(t.Context(), now.Add(-365*24*time.Hour))
	require.NoError(t, err)
	require.EqualValues(t, 2, pruned)

	left, err := db.Decisions(t.Context(), 10)
	require.NoError(t, err)
	require.Len(t, left, 1)
	require.Equal(t, now.Add(-time.Hour), left[0].At)
}

func TestSettings(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, found, err := db.Setting(t.Context(), "auth_server_id")
	require.NoError(t, err)
	require.False(t, found)

	require.NoError(t, db.SetSetting(t.Context(), "auth_server_id", "icloud.com"))
	require.NoError(t, db.SetSetting(t.Context(), "auth_server_id", "google.com"))

	value, found, err := db.Setting(t.Context(), "auth_server_id")
	require.NoError(t, err)
	require.True(t, found)
	require.Equal(t, "google.com", value)
}

func TestDecisionCorrectedBy(t *testing.T) {
	t.Parallel()

	db := open(t)
	at := time.Date(2026, 10, 29, 12, 0, 0, 0, time.UTC)

	record := func(messageID, reason string) int64 {
		at = at.Add(time.Minute)

		id, err := db.RecordDecision(t.Context(), store.Decision{
			At: at, Mailbox: inbox, MessageID: messageID, Verdict: verdictGood,
			Reason: reason, Action: actionNone,
		})
		require.NoError(t, err)

		return id
	}

	earlyFix := record("<m1>", "correction")
	judged := record("<m1>", reasonBayes)
	unrelated := record("<m2>", reasonBayes)
	providerJunk := record("<m3>", reasonBayes)
	record("<m3>", "provider junk")
	noID := record("", reasonBayes)
	record("", "correction")
	fix := record("<m1>", "correction")
	secondFix := record("<m1>", "undo")

	corrected := func(id int64) int64 {
		decision, err := db.Decision(t.Context(), id)
		require.NoError(t, err)

		return decision.CorrectedBy
	}

	require.Equal(t, fix, corrected(judged), "the first later correction, not the earlier one")
	require.Equal(t, fix, corrected(earlyFix), "a correction is corrected in turn")
	require.Zero(t, corrected(unrelated))
	require.Zero(t, corrected(providerJunk), "training from provider Junk is not a correction")
	require.Zero(t, corrected(noID), "decisions without a Message-ID are never linked")
	require.Equal(t, secondFix, corrected(fix), "an undo after a correction")

	listed, err := db.Decisions(t.Context(), 100)
	require.NoError(t, err)

	for _, decision := range listed {
		if decision.ID == judged {
			require.Equal(t, fix, decision.CorrectedBy)
		}
	}

	latest, found, err := db.LatestDecision(t.Context(), "<m1>")
	require.NoError(t, err)
	require.True(t, found)
	require.Equal(t, secondFix, latest.ID)
}

func TestUntrain(t *testing.T) {
	t.Parallel()

	db := open(t)

	_, err := db.Train(t.Context(), "m1", []string{"a", "b"}, classify.Good)
	require.NoError(t, err)
	_, err = db.Train(t.Context(), "m2", []string{"a"}, classify.Spam)
	require.NoError(t, err)

	found, err := db.Untrain(t.Context(), "m1")
	require.NoError(t, err)
	require.True(t, found)

	spam, good, totalSpam, totalGood := counts(t, db, "a")
	require.Equal(t, 1, spam, "m2 still counts")
	require.Zero(t, good)
	require.Equal(t, 1, totalSpam)
	require.Zero(t, totalGood)

	spam, good, _, _ = counts(t, db, "b")
	require.Zero(t, spam+good, "m1's own token is gone")

	_, trained, err := db.Trained(t.Context(), "m1")
	require.NoError(t, err)
	require.False(t, trained)

	found, err = db.Untrain(t.Context(), "m1")
	require.NoError(t, err)
	require.False(t, found, "nothing to reverse the second time")

	changed, err := db.Train(t.Context(), "m1", []string{"c"}, classify.Good)
	require.NoError(t, err)
	require.True(t, changed, "a message can be trained again, with new tokens")
}

func TestAddresses(t *testing.T) {
	t.Parallel()

	db := open(t)

	got, err := db.Addresses(t.Context())
	require.NoError(t, err)
	require.Empty(t, got)

	require.NoError(t, db.AddAddress(t.Context(), "B@Example.com", "sent"))
	require.NoError(t, db.AddAddress(t.Context(), "a@example.com", "manual"))

	got, err = db.Addresses(t.Context())
	require.NoError(t, err)
	require.Len(t, got, 2)
	require.Equal(t, "a@example.com", got[0].Address)
	require.Equal(t, "manual", got[0].Source)
	require.Equal(t, "b@example.com", got[1].Address)
	require.WithinDuration(t, time.Now(), got[1].AddedAt, time.Minute)
}

func TestPasskeys(t *testing.T) {
	t.Parallel()

	db := open(t)

	require.NoError(t, db.AddPasskey(t.Context(), "cred-1", "laptop", []byte(`{"n":1}`)))
	require.NoError(t, db.AddPasskey(t.Context(), "cred-2", "phone", []byte(`{"n":2}`)))
	require.ErrorIs(t, db.AddPasskey(t.Context(), "cred-1", "dup", []byte("{}")), store.ErrExists)

	keys, err := db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Len(t, keys, 2)
	require.Equal(t, "laptop", keys[0].Name)
	require.JSONEq(t, `{"n":1}`, string(keys[0].Credential))
	require.True(t, keys[0].LastUsed.IsZero())

	require.NoError(t, db.UpdatePasskey(t.Context(), "cred-1", []byte(`{"n":9}`)))

	keys, err = db.Passkeys(t.Context())
	require.NoError(t, err)
	require.JSONEq(t, `{"n":9}`, string(keys[0].Credential))
	require.WithinDuration(t, time.Now(), keys[0].LastUsed, time.Minute)

	require.ErrorIs(t, db.UpdatePasskey(t.Context(), "nope", []byte("{}")), store.ErrNotFound)

	require.NoError(t, db.RemovePasskey(t.Context(), keys[0].ID))
	require.ErrorIs(t, db.RemovePasskey(t.Context(), keys[0].ID), store.ErrNotFound)

	keys, err = db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Len(t, keys, 1)
}

func TestEnrollmentTokens(t *testing.T) {
	t.Parallel()

	now := time.Now()

	tests := []struct {
		name    string
		expires time.Time
		consume string
		at      time.Time
		want    bool
	}{
		{"valid", now.Add(time.Minute), "h", now, true},
		{"expired", now.Add(time.Minute), "h", now.Add(2 * time.Minute), false},
		{"unknown", now.Add(time.Minute), "other", now, false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			db := open(t)
			require.NoError(t, db.AddEnrollment(t.Context(), "h", tt.expires))

			got, err := db.ConsumeEnrollment(t.Context(), tt.consume, tt.at)
			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}

func TestEnrollmentTokenIsSingleUse(t *testing.T) {
	t.Parallel()

	db := open(t)
	now := time.Now()
	require.NoError(t, db.AddEnrollment(t.Context(), "h", now.Add(time.Minute)))

	first, err := db.ConsumeEnrollment(t.Context(), "h", now)
	require.NoError(t, err)
	require.True(t, first)

	second, err := db.ConsumeEnrollment(t.Context(), "h", now)
	require.NoError(t, err)
	require.False(t, second)
}

func TestAddEnrollmentDropsExpiredTokens(t *testing.T) {
	t.Parallel()

	db := open(t)
	past := time.Now().Add(-time.Hour)
	require.NoError(t, db.AddEnrollment(t.Context(), "old", past))
	require.NoError(t, db.AddEnrollment(t.Context(), "new", time.Now().Add(time.Minute)))

	got, err := db.ConsumeEnrollment(t.Context(), "old", past.Add(-time.Minute))
	require.NoError(t, err)
	require.False(t, got)
}

const testAddress = "a@example.com"

func TestParseAddress(t *testing.T) {
	t.Parallel()

	tests := []struct {
		in, want string
		ok       bool
	}{
		{testAddress, testAddress, true},
		{"  A@Example.COM ", testAddress, true},
		{"Alice <a@example.com>", testAddress, true},
		{"", "", false},
		{"not-an-address", "", false},
		{"a@b.com, c@d.com", "", false},
	}

	for _, tt := range tests {
		t.Run(tt.in, func(t *testing.T) {
			t.Parallel()

			got, err := store.ParseAddress(tt.in)
			if !tt.ok {
				require.ErrorIs(t, err, store.ErrInvalidAddress)

				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}

func TestWebMethodsFailOnAClosedStore(t *testing.T) {
	t.Parallel()

	db := open(t)
	require.NoError(t, db.Close())

	ctx := t.Context()
	now := time.Now()

	calls := map[string]func(*testing.T){
		"Addresses": func(t *testing.T) {
			t.Helper()

			_, err := db.Addresses(ctx)
			require.Error(t, err)
		},
		"AddPasskey": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.AddPasskey(ctx, "c", "n", []byte("{}")))
		},
		"Passkeys": func(t *testing.T) {
			t.Helper()

			_, err := db.Passkeys(ctx)
			require.Error(t, err)
		},
		"UpdatePasskey": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.UpdatePasskey(ctx, "c", []byte("{}")))
		},
		"RemovePasskey": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.RemovePasskey(ctx, 1))
		},
		"AddEnrollment": func(t *testing.T) {
			t.Helper()

			require.Error(t, db.AddEnrollment(ctx, "h", now))
		},
		"ConsumeEnrollment": func(t *testing.T) {
			t.Helper()

			_, err := db.ConsumeEnrollment(ctx, "h", now)
			require.Error(t, err)
		},
	}

	for name, call := range calls {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			call(t)
		})
	}
}

func TestJournalQueriesUseIndex(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		query string
	}{
		{"newest first", "SELECT * FROM decisions ORDER BY at DESC, id DESC LIMIT 10"},
		{"prune", "DELETE FROM decisions WHERE at < 1"},
		{"oldest", "SELECT MIN(at) FROM decisions"},
	}

	path := filepath.Join(t.TempDir(), "tamis.db")

	db, err := store.Open(t.Context(), path)
	require.NoError(t, err)
	require.NoError(t, db.Close())

	raw, err := sql.Open("sqlite", path)
	require.NoError(t, err)

	t.Cleanup(func() { require.NoError(t, raw.Close()) })

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			var plan string

			rows, err := raw.QueryContext(t.Context(), "EXPLAIN QUERY PLAN "+tt.query)
			require.NoError(t, err)

			defer func() { require.NoError(t, rows.Close()) }()

			var planSb970 strings.Builder

			for rows.Next() {
				var (
					id, parent, unused int
					detail             string
				)

				require.NoError(t, rows.Scan(&id, &parent, &unused, &detail))

				planSb970.WriteString(detail + "\n")
			}

			plan += planSb970.String()

			require.NoError(t, rows.Err())
			require.NotContains(t, plan, "SCAN decisions\n", tt.query)
			require.Contains(t, plan, "decisions_by_at")
		})
	}
}

func TestUntrainLeavesUnrelatedTokensAlone(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "tamis.db")

	db, err := store.Open(t.Context(), path)
	require.NoError(t, err)

	defer func() { require.NoError(t, db.Close()) }()

	_, err = db.Train(t.Context(), "m1", []string{"gamma"}, classify.Spam)
	require.NoError(t, err)

	raw, err := sql.Open("sqlite", path)
	require.NoError(t, err)

	defer func() { require.NoError(t, raw.Close()) }()

	_, err = raw.ExecContext(
		t.Context(),
		"INSERT INTO tokens (token, spam, good) VALUES ('stale', 0, 0)",
	)
	require.NoError(t, err)

	removed, err := db.Untrain(t.Context(), "m1")
	require.NoError(t, err)
	require.True(t, removed)

	var tokens []string

	rows, err := raw.QueryContext(t.Context(), "SELECT token FROM tokens ORDER BY token")
	require.NoError(t, err)

	defer func() { require.NoError(t, rows.Close()) }()

	for rows.Next() {
		var token string

		require.NoError(t, rows.Scan(&token))

		tokens = append(tokens, token)
	}

	require.NoError(t, rows.Err())
	require.Equal(t, []string{"stale"}, tokens)
}

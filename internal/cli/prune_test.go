package cli

import (
	"bytes"
	"log/slog"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

func TestJournalPrunerRunsOncePerDay(t *testing.T) {
	t.Parallel()

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = db.Close() })

	start := time.Date(2026, 10, 29, 12, 0, 0, 0, time.UTC)
	retention := 30 * 24 * time.Hour

	addOld := func() {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At: start.Add(-2 * retention), Mailbox: "INBOX", MessageID: "<m>",
			Verdict: "good", Reason: "bayes", Action: "none",
		})
		require.NoError(t, err)
	}
	count := func() int {
		stats, err := db.Stats(t.Context())
		require.NoError(t, err)

		return stats.Decisions
	}

	var pruner journalPruner

	logger := slog.New(slog.DiscardHandler)

	addOld()
	require.NoError(t, pruner.prune(t.Context(), db, retention, start, logger))
	require.Zero(t, count(), "the first call prunes")

	addOld()
	require.NoError(t, pruner.prune(t.Context(), db, retention, start.Add(time.Hour), logger))
	require.Equal(t, 1, count(), "not again within a day")

	require.NoError(t, pruner.prune(t.Context(), db, retention, start.Add(25*time.Hour), logger))
	require.Zero(t, count(), "again after a day")

	var forever journalPruner

	addOld()
	require.NoError(t, forever.prune(t.Context(), db, 0, start, logger))
	require.Equal(t, 1, count(), "zero retention keeps everything")
}

func TestJournalPrunerLogsAtDebug(t *testing.T) {
	t.Parallel()

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = db.Close() })

	var out bytes.Buffer

	logger := slog.New(slog.NewTextHandler(&out, &slog.HandlerOptions{Level: slog.LevelDebug}))
	now := time.Date(2026, 10, 29, 12, 0, 0, 0, time.UTC)

	var pruner journalPruner

	require.NoError(t, pruner.prune(t.Context(), db, time.Hour, now, logger))
	require.Contains(t, out.String(), `msg="journal prune"`)
	require.Contains(t, out.String(), "decisions=0")

	out.Reset()

	require.NoError(t, pruner.prune(t.Context(), db, time.Hour, now.Add(time.Minute), logger))
	require.Contains(t, out.String(), `msg="journal prune skipped"`)
}

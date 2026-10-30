package live_test

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/live"
	"github.com/stretchr/testify/require"
)

var errBoom = errors.New("boom")

func TestStatus(t *testing.T) {
	t.Parallel()

	started := time.Date(2026, 10, 30, 9, 0, 0, 0, time.UTC)
	status := live.NewStatus(started)

	snap := status.Snapshot()
	require.Equal(t, started, snap.Started)
	require.Zero(t, snap.Passes)
	require.True(t, snap.LastPass.IsZero())

	pass := started.Add(time.Minute)

	status.Wait(live.Running)
	require.Equal(t, live.Running, status.Snapshot().Waiting)

	status.PassDone(pass, "scanned 3, spam 1")
	status.Wait(live.Idle)

	snap = status.Snapshot()
	require.Equal(t, 1, snap.Passes)
	require.Equal(t, pass, snap.LastPass)
	require.Equal(t, "scanned 3, spam 1", snap.LastSummary)
	require.Equal(t, live.Idle, snap.Waiting)
	require.Empty(t, snap.LastError)

	failedAt := pass.Add(time.Minute)
	status.Failed(failedAt, errBoom)

	snap = status.Snapshot()
	require.Equal(t, "boom", snap.LastError)
	require.Equal(t, failedAt, snap.LastErrorAt)
	require.Equal(t, 1, snap.Passes, "a failure is not a pass")
	require.Equal(t, "scanned 3, spam 1", snap.LastSummary, "the last good pass is kept")
}

func TestNilStatusIsHarmless(t *testing.T) {
	t.Parallel()

	var status *live.Status

	status.PassDone(time.Now(), "x")
	status.Failed(time.Now(), errBoom)
	status.Wait(live.Polling)
	require.Zero(t, status.Snapshot())
}

func TestStatusIsSafeForConcurrentUse(t *testing.T) {
	t.Parallel()

	status := live.NewStatus(time.Now())

	var wg sync.WaitGroup

	for range 20 {
		wg.Go(func() {
			status.PassDone(time.Now(), "x")
			status.Wait(live.Polling)
			_ = status.Snapshot()
		})
	}

	wg.Wait()
	require.Equal(t, 20, status.Snapshot().Passes)
}

// quiet logs everything into a buffer nobody reads. slog.DiscardHandler
// reports every level as disabled, which would starve the ring.
func quiet() slog.Handler {
	return slog.NewTextHandler(new(bytes.Buffer), &slog.HandlerOptions{Level: slog.LevelDebug})
}

func texts(entries []live.Entry) []string {
	out := make([]string, len(entries))
	for i, entry := range entries {
		out[i] = entry.Text
	}

	return out
}

func TestRingKeepsTheNewestEntriesFirst(t *testing.T) {
	t.Parallel()

	ring := live.NewRing(3)
	logger := slog.New(ring.Handler(quiet()))

	for _, msg := range []string{"m1", "m2", "m3", "m4", "m5"} {
		logger.Info(msg)
	}

	require.Equal(t, []string{"m5", "m4", "m3"}, texts(ring.Entries()))
}

func TestRingBeforeItFills(t *testing.T) {
	t.Parallel()

	ring := live.NewRing(5)
	logger := slog.New(ring.Handler(quiet()))

	require.Empty(t, ring.Entries())

	logger.Warn("a")
	logger.Error("b")

	entries := ring.Entries()
	require.Equal(t, []string{"b", "a"}, texts(entries))
	require.Equal(t, slog.LevelError, entries[0].Level)
	require.False(t, entries[0].Time.IsZero())
}

func TestRingRecordsAttributes(t *testing.T) {
	t.Parallel()

	ring := live.NewRing(5)
	logger := slog.New(ring.Handler(quiet()))

	logger.Error("pass failed", "err", errBoom, "folder", "INBOX")
	logger.With("account", "me").WithGroup("g").Info("x", "k", "v")

	require.Equal(t, []string{
		"x account=me g.k=v",
		"pass failed err=boom folder=INBOX",
	}, texts(ring.Entries()))
}

func TestRingPassesRecordsOnAndFollowsItsLevel(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer

	next := slog.NewTextHandler(&out, &slog.HandlerOptions{Level: slog.LevelWarn})
	ring := live.NewRing(5)
	logger := slog.New(ring.Handler(next))

	logger.Info("quiet")
	logger.Warn("loud")

	require.Equal(
		t,
		[]string{"loud"},
		texts(ring.Entries()),
		"what the logger drops, the ring drops",
	)
	require.Contains(t, out.String(), "loud")
	require.NotContains(t, out.String(), "quiet")
	require.True(t, ring.Handler(next).Enabled(context.Background(), slog.LevelError))
	require.False(t, ring.Handler(next).Enabled(context.Background(), slog.LevelInfo))
}

func TestNilRingIsHarmless(t *testing.T) {
	t.Parallel()

	var ring *live.Ring

	require.Empty(t, ring.Entries())

	next := quiet()
	require.Same(t, next, ring.Handler(next), "without a ring, logging is untouched")
}

func TestRingIsSafeForConcurrentUse(t *testing.T) {
	t.Parallel()

	ring := live.NewRing(10)
	logger := slog.New(ring.Handler(quiet()))

	var wg sync.WaitGroup

	for range 20 {
		wg.Go(func() {
			logger.Info("x")

			_ = ring.Entries()
		})
	}

	wg.Wait()
	require.Len(t, ring.Entries(), 10)
}

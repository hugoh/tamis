package retry_test

import (
	"log/slog"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/retry"
	"github.com/stretchr/testify/require"
)

func TestBackoffGrowsToItsLimitAndResets(t *testing.T) {
	t.Parallel()

	bo := retry.Backoff(time.Second, 10*time.Second)

	var last time.Duration

	for range 10 {
		last = bo.NextBackOff()
		require.LessOrEqual(t, last, 12*time.Second, "the limit, plus its jitter")
	}

	require.GreaterOrEqual(t, last, 8*time.Second, "it settles near the limit")

	bo.Reset()
	require.LessOrEqual(t, bo.NextBackOff(), 1200*time.Millisecond, "a success starts it over")
}

func TestBackoffLimitNeverBelowFirst(t *testing.T) {
	t.Parallel()

	bo := retry.Backoff(time.Minute, time.Second)
	require.GreaterOrEqual(t, bo.NextBackOff(), 48*time.Second)
}

func TestStreakTracksFailuresUntilRecovery(t *testing.T) {
	t.Parallel()

	var streak retry.Streak

	start := time.Date(2026, 1, 1, 12, 0, 0, 0, time.UTC)

	_, _, ok := streak.Recover(start)
	require.False(t, ok, "nothing to recover from")
	require.True(t, streak.Since().IsZero())

	require.Equal(t, 1, streak.Fail(start))
	require.Equal(t, 2, streak.Fail(start.Add(time.Minute)))
	require.Equal(t, start, streak.Since())

	failures, downtime, ok := streak.Recover(start.Add(10 * time.Minute))
	require.True(t, ok)
	require.Equal(t, 2, failures)
	require.Equal(t, 10*time.Minute, downtime)
	require.Equal(t, 1, streak.Fail(start.Add(time.Hour)), "a new streak starts over")
}

func TestNotableFailuresThinOut(t *testing.T) {
	t.Parallel()

	var got []int

	for n := 1; n <= 41; n++ {
		if retry.Notable(n) {
			got = append(got, n)
		}
	}

	require.Equal(t, []int{1, 2, 3, 5, 10, 20, 40}, got)
}

func TestLevelsThinOut(t *testing.T) {
	t.Parallel()

	require.Equal(t, slog.LevelError, retry.Level(1, slog.LevelError))
	require.Equal(t, slog.LevelWarn, retry.Level(2, slog.LevelError))
	require.Equal(t, slog.LevelDebug, retry.Level(4, slog.LevelError))

	require.Equal(t, slog.LevelError, retry.RejectedLevel(1))
	require.Equal(t, slog.LevelDebug, retry.RejectedLevel(3))
	require.Equal(t, slog.LevelWarn, retry.RejectedLevel(4))
}

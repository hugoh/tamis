package retry_test

import (
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/hugoh/tamis/internal/retry"
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

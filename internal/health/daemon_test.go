package health_test

import (
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/live"
	"github.com/stretchr/testify/require"
)

func TestDaemonCheck(t *testing.T) {
	t.Parallel()

	now := time.Date(2026, 1, 1, 12, 0, 0, 0, time.UTC)
	poll := 2 * time.Minute

	tests := []struct {
		name   string
		snap   *live.Snapshot
		status health.Status
		detail string
	}{
		{"never saved", nil, health.StatusSkip, "has 'tamis run' ever run"},
		{
			"no pass yet",
			&live.Snapshot{Started: now.Add(-time.Minute), Updated: now.Add(-time.Minute)},
			health.StatusOK, "no pass yet",
		},
		{
			"healthy",
			&live.Snapshot{
				Passes: 3, LastPass: now.Add(-time.Minute), Updated: now.Add(-time.Minute),
				LastSummary: "scanned 2",
			},
			health.StatusOK, "last pass 1m0s ago: scanned 2",
		},
		{
			"recovered",
			&live.Snapshot{
				Passes: 5, LastPass: now.Add(-time.Minute), Updated: now.Add(-time.Minute),
				RecoveredAt: now.Add(-time.Minute), LastErrorAt: now.Add(-time.Hour),
			},
			health.StatusOK, "recovered 1m0s ago",
		},
		{
			"failing",
			&live.Snapshot{
				Failures: 3, FailingSince: now.Add(-time.Hour), LastError: "connection refused",
				Updated: now.Add(-time.Minute), RetryAt: now.Add(time.Minute),
			},
			health.StatusWarn, "3 passes failed in a row",
		},
		{
			"login rejected",
			&live.Snapshot{
				Failures: 2, Rejected: true, FailingSince: now.Add(-time.Hour),
				Updated: now.Add(-time.Minute), RetryAt: now.Add(10 * time.Minute),
			},
			health.StatusFail, "rejects the login",
		},
		{
			"gone quiet",
			&live.Snapshot{Passes: 1, LastPass: now.Add(-time.Hour), Updated: now.Add(-time.Hour)},
			health.StatusWarn, "is 'tamis run' going",
		},
		{
			"waiting out a long back-off is not quiet",
			&live.Snapshot{
				Failures: 9, FailingSince: now.Add(-3 * time.Hour), LastError: "down",
				Updated: now.Add(-10 * time.Minute), RetryAt: now.Add(5 * time.Minute),
			},
			health.StatusWarn, "9 passes failed in a row",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			status, detail := health.DaemonInfo{Saved: tt.snap, Poll: poll}.Describe(now)
			require.Equal(t, tt.status, status, detail)
			require.Contains(t, detail, tt.detail)
		})
	}
}

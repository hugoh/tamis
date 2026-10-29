package health_test

import (
	"bytes"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

func now() time.Time { return time.Date(2026, 10, 29, 12, 0, 0, 0, time.UTC) }

// decisions builds n entries, newest first, ageing from the given age.
func decisions(n int, age time.Duration, verdict, reason string) []store.Decision {
	out := make([]store.Decision, n)
	for i := range out {
		out[i] = store.Decision{
			At:      now().Add(-age - time.Duration(i)*time.Minute),
			Verdict: verdict,
			Reason:  reason,
		}
	}

	return out
}

func TestRun(t *testing.T) {
	t.Parallel()

	balanced := store.Stats{Spam: 400, Good: 500}

	tests := []struct {
		name   string
		stats  store.Stats
		recent []store.Decision
		check  string
		want   health.Status
	}{
		{"balanced model", balanced, nil, health.NameBalance, health.StatusOK},
		{"no good examples", store.Stats{Spam: 3000}, nil, health.NameBalance, health.StatusFail},
		{"no spam examples", store.Stats{Good: 3000}, nil, health.NameBalance, health.StatusFail},
		{
			"too few of one kind",
			store.Stats{Spam: 10, Good: 500},
			nil,
			health.NameBalance,
			health.StatusWarn,
		},
		{
			"lopsided",
			store.Stats{Spam: 6000, Good: 500},
			nil,
			health.NameBalance,
			health.StatusWarn,
		},
		{
			"too few decisions to judge",
			balanced, decisions(5, time.Minute, engine.VerdictUncertain, engine.ReasonBayes),
			health.NameUncertain, health.StatusSkip,
		},
		{
			"mostly decided",
			balanced, decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes),
			health.NameUncertain, health.StatusOK,
		},
		{
			"mostly unsure",
			balanced, decisions(30, time.Minute, engine.VerdictUncertain, engine.ReasonBayes),
			health.NameUncertain, health.StatusWarn,
		},
		{
			"no corrections",
			balanced, decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes),
			health.NameCorrections, health.StatusOK,
		},
		{
			"many corrections",
			balanced,
			append(
				decisions(10, time.Minute, engine.VerdictGood, engine.ReasonCorrection),
				decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes)...,
			),
			health.NameCorrections, health.StatusWarn,
		},
		{
			"provider junk training is not a correction",
			balanced,
			append(
				decisions(10, time.Minute, engine.VerdictSpam, engine.ReasonProviderJunk),
				decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes)...,
			),
			health.NameCorrections, health.StatusOK,
		},
		{
			"recent decision",
			balanced,
			decisions(1, time.Hour, "good", engine.ReasonBayes),
			health.NameLast,
			health.StatusOK,
		},
		{
			"stale decision",
			balanced,
			decisions(1, 48*time.Hour, "good", engine.ReasonBayes),
			health.NameLast,
			health.StatusWarn,
		},
		{"no decisions yet", balanced, nil, health.NameLast, health.StatusWarn},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			report := health.Run(health.Input{Stats: tt.stats, Recent: tt.recent, Now: now()})

			for _, check := range report.Checks {
				if check.Name == tt.check {
					require.Equal(t, tt.want, check.Status, check.Detail)

					return
				}
			}

			require.Failf(t, "missing check", "%q not in report", tt.check)
		})
	}
}

func TestFailedOnlyForFailures(t *testing.T) {
	t.Parallel()

	warned := health.Run(health.Input{Stats: store.Stats{Spam: 10, Good: 500}, Now: now()})
	require.False(t, warned.Failed())

	failed := health.Run(health.Input{Stats: store.Stats{Spam: 3000}, Now: now()})
	require.True(t, failed.Failed())
}

func TestRender(t *testing.T) {
	t.Parallel()

	report := health.Run(health.Input{Stats: store.Stats{Spam: 3000}, Now: now()})

	var text bytes.Buffer

	require.NoError(t, report.WriteText(&text))
	require.Contains(t, text.String(), "FAIL")
	require.Contains(t, text.String(), "class balance")

	var asJSON bytes.Buffer

	require.NoError(t, report.WriteJSON(&asJSON))
	require.Contains(t, asJSON.String(), `"status": "fail"`, asJSON.String())
}

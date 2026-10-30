package health_test

import (
	"bytes"
	"net"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const icloudDomain = "icloud.com"

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

// corrected marks the first n decisions as later corrected.
func corrected(list []store.Decision, n int) []store.Decision {
	for i := range n {
		list[i].CorrectedBy = int64(1000 + i)
	}

	return list
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
			"many judged decisions were corrected",
			balanced,
			corrected(decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes), 10),
			health.NameCorrections,
			health.StatusWarn,
		},
		{
			"a few were corrected",
			balanced,
			corrected(decisions(30, time.Minute, engine.VerdictGood, engine.ReasonBayes), 2),
			health.NameCorrections,
			health.StatusOK,
		},
		{
			"correction rows alone are not corrected decisions",
			balanced,
			append(
				decisions(10, time.Minute, engine.VerdictSpam, engine.ReasonCorrection),
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

func TestJournalChecks(t *testing.T) {
	t.Parallel()

	recent := corrected(
		append(
			decisions(3, time.Minute, engine.VerdictGood, engine.ReasonBayes),
			decisions(2, time.Minute, engine.VerdictSpam, engine.ReasonBayes)...,
		),
		1,
	)
	recent = append(
		recent,
		decisions(1, time.Minute, engine.VerdictGood, engine.ReasonCorrection)...,
	)

	tests := []struct {
		name      string
		stats     store.Stats
		retention time.Duration
		recent    []store.Decision
		check     string
		want      health.Status
		contains  []string
	}{
		{
			name: "journal span and retention",
			stats: store.Stats{
				Decisions:      6,
				OldestDecision: time.Date(2026, 10, 28, 9, 0, 0, 0, time.UTC),
			},
			retention: 365 * 24 * time.Hour,
			check:     health.NameJournal,
			want:      health.StatusOK,
			contains:  []string{"6 decisions", "since 2026-10-28", "kept 365 days"},
		},
		{
			name:     "journal kept forever",
			stats:    store.Stats{Decisions: 1, OldestDecision: now()},
			check:    health.NameJournal,
			want:     health.StatusOK,
			contains: []string{"kept forever"},
		},
		{
			name:  "empty journal",
			check: health.NameJournal,
			want:  health.StatusSkip, contains: []string{"empty"},
		},
		{
			name:     "recent verdicts",
			recent:   recent,
			check:    health.NameRecent,
			want:     health.StatusOK,
			contains: []string{"last 6", "3 good", "2 spam", "0 uncertain", "1 corrected"},
		},
		{
			name:  "no recent verdicts",
			check: health.NameRecent,
			want:  health.StatusSkip,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			report := health.Run(health.Input{
				Stats: tt.stats, Recent: tt.recent, Retention: tt.retention, Now: now(),
			})

			for _, check := range report.Checks {
				if check.Name == tt.check {
					require.Equal(t, tt.want, check.Status, check.Detail)

					for _, want := range tt.contains {
						require.Contains(t, check.Detail, want)
					}

					return
				}
			}

			require.Failf(t, "missing check", "%q not in report", tt.check)
		})
	}
}

func TestAuthPinCheck(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name     string
		pin      engine.AuthPin
		want     health.Status
		contains string
	}{
		{
			"learnt",
			engine.AuthPin{ID: icloudDomain, Source: engine.AuthLearnt},
			health.StatusOK,
			icloudDomain,
		},
		{
			"configured",
			engine.AuthPin{ID: icloudDomain, Source: engine.AuthConfigured},
			health.StatusOK,
			"configured",
		},
		{"switched off", engine.AuthPin{Source: engine.AuthOff}, health.StatusOK, "off"},
		{"not learnt", engine.AuthPin{Source: engine.AuthUnset}, health.StatusSkip, "bootstrap"},
		{
			"learnt for another host",
			engine.AuthPin{
				Source:    engine.AuthStale,
				LearntFor: "imap.mail.me.com",
				Host:      "imap.gmail.com",
			},
			health.StatusWarn,
			"imap.mail.me.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			report := health.Run(health.Input{Auth: tt.pin, Now: now()})

			for _, check := range report.Checks {
				if check.Name == health.NameAuthID {
					require.Equal(t, tt.want, check.Status, check.Detail)
					require.Contains(t, check.Detail, tt.contains)

					return
				}
			}

			require.Fail(t, "missing check")
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

func TestCollectReadsTheStore(t *testing.T) {
	t.Parallel()

	database, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = database.Close() })

	_, err = database.RecordDecision(t.Context(), store.Decision{
		At: time.Now(), Verdict: engine.VerdictGood, Reason: "bayes", Action: "none",
	})
	require.NoError(t, err)

	report, err := health.Collect(t.Context(), database, &config.Config{})
	require.NoError(t, err)

	names := make([]string, 0, len(report.Checks))
	for _, check := range report.Checks {
		names = append(names, check.Name)
	}

	require.Contains(t, names, health.NameBalance)
	require.Contains(t, names, health.NameLast)
}

func TestCollectFailsOnAClosedStore(t *testing.T) {
	t.Parallel()

	database, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	require.NoError(t, database.Close())

	_, err = health.Collect(t.Context(), database, &config.Config{})
	require.Error(t, err)
}

func webCheck(t *testing.T, name string, web health.Web) health.Check {
	t.Helper()

	for _, check := range health.Run(health.Input{Now: now(), Web: web}).Checks {
		if check.Name == name {
			return check
		}
	}

	t.Fatalf("no check named %q", name)

	return health.Check{}
}

func TestWebChecks(t *testing.T) {
	t.Parallel()

	const listen = "127.0.0.1:8484"

	tests := []struct {
		name       string
		web        health.Web
		check      string
		wantStatus health.Status
		wantDetail string
	}{
		{"disabled", health.Web{}, health.NameWeb, health.StatusSkip, "not enabled"},
		{
			"listening",
			health.Web{
				Enabled:   true,
				Listen:    listen,
				Origin:    "https://tamis.example.net",
				Reachable: true,
			},
			health.NameWeb,
			health.StatusOK,
			"listening on " + listen + ", at https://tamis.example.net",
		},
		{
			"not listening",
			health.Web{Enabled: true, Listen: listen},
			health.NameWeb, health.StatusWarn, "nothing is listening on " + listen,
		},
		{
			"no passkeys",
			health.Web{Enabled: true, Listen: listen},
			health.NamePasskeys, health.StatusWarn, "tamis web enroll",
		},
		{
			"passkeys",
			health.Web{Enabled: true, Listen: listen, Passkeys: 2},
			health.NamePasskeys, health.StatusOK, "2 enrolled",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			check := webCheck(t, tt.check, tt.web)
			require.Equal(t, tt.wantStatus, check.Status)
			require.Contains(t, check.Detail, tt.wantDetail)
		})
	}
}

func TestPasskeysCheckOnlyWhenWebIsEnabled(t *testing.T) {
	t.Parallel()

	for _, check := range health.Run(health.Input{Now: now()}).Checks {
		require.NotEqual(t, health.NamePasskeys, check.Name)
	}
}

func TestCollectChecksTheWebInterface(t *testing.T) {
	t.Parallel()

	var lc net.ListenConfig

	listener, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)
	t.Cleanup(func() { _ = listener.Close() })

	database, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = database.Close() })
	require.NoError(t, database.AddPasskey(t.Context(), "c1", "laptop", []byte("{}")))

	cfg := &config.Config{Web: config.Web{
		Enabled: true, Listen: listener.Addr().String(), Origin: "http://localhost:8484",
	}}

	report, err := health.Collect(t.Context(), database, cfg)
	require.NoError(t, err)
	require.Contains(t, report.Checks, health.Check{
		Name: health.NameWeb, Status: health.StatusOK,
		Detail: "listening on " + listener.Addr().String() + ", at http://localhost:8484",
	})
	require.Contains(t, report.Checks, health.Check{
		Name: health.NamePasskeys, Status: health.StatusOK, Detail: "1 enrolled",
	})

	require.NoError(t, listener.Close())

	report, err = health.Collect(t.Context(), database, cfg)
	require.NoError(t, err)

	var web health.Check

	for _, check := range report.Checks {
		if check.Name == health.NameWeb {
			web = check
		}
	}

	require.Equal(
		t,
		health.StatusWarn,
		web.Status,
		"an unreachable web interface is a warning, not a failure",
	)
}

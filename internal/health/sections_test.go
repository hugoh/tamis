package health_test

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const testScript = "/r.sieve"

var errBadScript = errors.New("line 3: unknown command")

func checkNamed(t *testing.T, report *health.Report, name string) health.Check {
	t.Helper()

	for _, check := range report.Checks {
		if check.Name == name {
			return check
		}
	}

	t.Fatalf("no check named %q", name)

	return health.Check{}
}

func TestChecksAreGroupedIntoSectionsInOrder(t *testing.T) {
	t.Parallel()

	report := health.Run(health.Input{
		Stats: store.Stats{Spam: 400, Good: 500},
		Web:   health.Web{Enabled: true, Reachable: true, Passkeys: 1},
		Now:   now(),
	})

	var seen []string

	for _, check := range report.Checks {
		require.NotEmpty(t, check.Section, check.Name)

		if !slices.Contains(seen, check.Section) {
			seen = append(seen, check.Section)
		} else {
			require.Equal(
				t,
				seen[len(seen)-1],
				check.Section,
				"%s is out of its section",
				check.Name,
			)
		}
	}

	require.Equal(t, []string{
		health.SectionConfig, health.SectionRules, health.SectionTraining,
		health.SectionDecisions, health.SectionWeb,
	}, seen)
}

func TestConfigCheck(t *testing.T) {
	t.Parallel()

	report := health.Run(health.Input{
		Config: health.ConfigInfo{
			Path:    "/home/me/.config/tamis/config.toml",
			Account: "me@example.net on imap.example.net",
			Mode:    "live",
		},
		Now: now(),
	})

	check := checkNamed(t, report, health.NameConfig)
	require.Equal(t, health.StatusOK, check.Status)
	require.Contains(t, check.Detail, "/home/me/.config/tamis/config.toml")
	require.Contains(t, check.Detail, "me@example.net on imap.example.net")
	require.Contains(t, check.Detail, "live mode")
}

func TestRulesCheck(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		rules  health.RulesInfo
		status health.Status
		detail string
	}{
		{"not configured", health.RulesInfo{}, health.StatusSkip, "not configured"},
		{
			"valid",
			health.RulesInfo{Script: testScript, Targets: []string{"code", "comms"}},
			health.StatusOK, "2 folders: code, comms",
		},
		{
			"one folder",
			health.RulesInfo{Script: testScript, Targets: []string{"code"}},
			health.StatusOK, "1 folder: code",
		},
		{
			"invalid",
			health.RulesInfo{Script: testScript, Err: errBadScript},
			health.StatusFail, "line 3: unknown command",
		},
		{
			"files nothing",
			health.RulesInfo{Script: testScript},
			health.StatusWarn, "no fileinto",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			check := checkNamed(
				t,
				health.Run(health.Input{Rules: tt.rules, Now: now()}),
				health.NameRules,
			)
			require.Equal(t, tt.status, check.Status)
			require.Contains(t, check.Detail, tt.detail)
			require.Equal(t, health.SectionRules, check.Section)
		})
	}
}

func TestConfigFailedReport(t *testing.T) {
	t.Parallel()

	report := health.ConfigFailed(errBadScript, false)
	require.True(t, report.Failed())
	require.Len(t, report.Checks, 1)
	require.Equal(t, health.NameConfig, report.Checks[0].Name)
	require.Equal(t, health.SectionConfig, report.Checks[0].Section)
	require.Contains(t, report.Checks[0].Detail, "unknown command")

	report = health.ConfigFailed(errBadScript, true)
	require.Equal(t, health.NameRules, report.Checks[0].Name)
	require.Equal(t, health.SectionRules, report.Checks[0].Section)
}

func TestJSONCarriesTheSection(t *testing.T) {
	t.Parallel()

	var asJSON bytes.Buffer

	report := health.Run(health.Input{Stats: store.Stats{Spam: 400, Good: 500}, Now: now()})
	require.NoError(t, report.WriteJSON(&asJSON))
	require.Contains(t, asJSON.String(), `"section": "Training"`)
}

func TestCollectChecksTheConfigAndRules(t *testing.T) {
	t.Parallel()

	database, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = database.Close() })

	script := filepath.Join(t.TempDir(), "rules.sieve")
	require.NoError(t, os.WriteFile(script, []byte(`require "fileinto"; fileinto "code";`), 0o600))

	cfg := &config.Config{
		Path:    "/etc/tamis.toml",
		Account: config.Account{Host: "imap.example.net", User: "me@example.net"},
		Policy:  config.Policy{Mode: config.ModeLive},
		Rules:   config.Rules{Script: script},
	}

	report, err := health.Collect(t.Context(), database, cfg)
	require.NoError(t, err)
	require.Contains(t, checkNamed(t, report, health.NameConfig).Detail, "/etc/tamis.toml")
	require.Contains(t, checkNamed(t, report, health.NameRules).Detail, "1 folder: code")

	require.NoError(t, os.Remove(script))

	report, err = health.Collect(t.Context(), database, cfg)
	require.NoError(t, err)
	require.Equal(t, health.StatusFail, checkNamed(t, report, health.NameRules).Status)
}

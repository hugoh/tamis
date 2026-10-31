package cli_test

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func (a *account) runUnderSystemd(t *testing.T, systemd bool, args ...string) result {
	t.Helper()

	env := passwordEnv()
	if systemd {
		env["JOURNAL_STREAM"] = "8:12345"
	}

	return execute(t.Context(), t, invocation{
		args: append(args, configFlag, a.config), env: env, srv: a.srv,
	})
}

func TestLogsCarryJournalPrioritiesUnderSystemd(t *testing.T) {
	t.Parallel()

	a := withRules(t, "require [\"fileinto\"];\nfileinto \"Nope\";\n")

	got := a.runUnderSystemd(t, true, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)

	var warned, informed bool

	for line := range strings.Lines(strings.TrimSpace(got.stderr)) {
		require.Regexp(t, `^<[0-7]>`, line)
		require.NotContains(t, line, "time=", "journald adds its own timestamp")

		warned = warned || (strings.HasPrefix(line, "<4>") && strings.Contains(line, "target=Nope"))
		informed = informed ||
			(strings.HasPrefix(line, "<6>") && strings.Contains(line, "pass complete"))
	}

	require.True(t, warned, "a warning is priority 4:\n%s", got.stderr)
	require.True(t, informed, "info is priority 6:\n%s", got.stderr)
}

func TestLogsAreUnchangedOutsideSystemd(t *testing.T) {
	t.Parallel()

	a := withRules(t, "require [\"fileinto\"];\nfileinto \"Nope\";\n")

	got := a.runUnderSystemd(t, false, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stderr, "time=")
	require.Contains(t, got.stderr, "level=WARN")
	require.NotRegexp(t, `(?m)^<[0-7]>`, got.stderr)
}

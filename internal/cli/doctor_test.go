package cli_test

import (
	"fmt"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const (
	doctorCmd   = "doctor"
	doctorColor = "--color"
)

func TestDoctorGroupsChecksIntoSections(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	got := a.run(t, doctorCmd, doctorColor, "never")

	require.True(t, strings.HasPrefix(got.stdout, "Configuration\n"))

	for _, heading := range []string{"Rules", "Training", "Decisions", "Web interface"} {
		require.Contains(t, got.stdout, "\n"+heading+"\n")
	}

	require.Contains(t, got.stdout, "\n\nRules\n", "sections are separated by a blank line")
	require.Contains(t, got.stdout, "  FAIL  class balance")
	require.Contains(t, got.stdout, a.config)
	require.Contains(t, got.stdout, "shadow mode")
	require.Contains(t, got.stdout, "not configured")
}

func TestDoctorColoursStatuses(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	require.Contains(t, a.run(t, doctorCmd, doctorColor, "always").stdout, "\x1b[1;31mFAIL")
	require.NotContains(t, a.run(t, doctorCmd, doctorColor, "never").stdout, "\x1b[")
}

func TestDoctorChecksTheRules(t *testing.T) {
	t.Parallel()

	got := withRules(t, rulesScript).run(t, doctorCmd, doctorColor, "never")
	require.Contains(t, got.stdout, "files into 1 folder: Lists/Go")
	require.Contains(t, got.stdout, "rules.sieve")
}

func TestDoctorReportsAConfigThatDoesNotLoad(t *testing.T) {
	t.Parallel()

	got := execute(t.Context(), t, invocation{args: []string{doctorCmd, configFlag, missingConfig}})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stdout, "Configuration")
	require.Contains(t, got.stdout, "FAIL")
	require.Contains(t, got.stdout, readConfigFailed)
}

func TestDoctorReportsABadRulesScriptUnderRules(t *testing.T) {
	t.Parallel()

	script := writeFile(t, "bad.sieve", `fileinto "X";`)
	a := newAccount(t, fmt.Sprintf("\n[rules]\nscript = %q\n", script))

	got := a.run(t, doctorCmd, doctorColor, "never")
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stdout, "Rules\n")
	require.Contains(t, got.stdout, "FAIL  filing rules")
	require.Contains(t, got.stdout, "rules.script")
	require.NotContains(t, got.stdout, "class balance", "nothing else is judged on a broken config")
}

func TestDoctorJSONHasSections(t *testing.T) {
	t.Parallel()

	got := newAccount(t, "").run(t, doctorCmd, "--json")
	require.Contains(t, got.stdout, `"section": "Configuration"`)
	require.Contains(t, got.stdout, `"name": "config file"`)
}

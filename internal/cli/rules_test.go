package cli_test

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

const (
	rulesCmd     = "rules"
	rulesScript  = "require [\"fileinto\"];\nif header :contains \"list-id\" \"golang\" { fileinto \"Lists/Go\"; }\n"
	listMessage  = "From: Gopher <g@lists.example>\r\nList-Id: <golang-nuts.example>\r\nSubject: hi\r\n\r\nbody\r\n"
	plainMessage = "From: a@b.example\r\nSubject: hi\r\n\r\nbody\r\n"
)

func writeFile(t *testing.T, name, body string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), name)
	require.NoError(t, os.WriteFile(path, []byte(body), 0o600))

	return path
}

func withRules(t *testing.T, script string) *account {
	t.Helper()

	path := writeFile(t, "rules.sieve", script)

	return newAccount(t, fmt.Sprintf("\n[rules]\nscript = %q\n", path))
}

func TestRulesShow(t *testing.T) {
	t.Parallel()

	got := newAccount(t, "").run(t, rulesCmd, "show")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No rules script configured")

	got = withRules(t, rulesScript).run(t, rulesCmd, "show")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "rules.sieve")
	require.Contains(t, got.stdout, `fileinto "Lists/Go"`)
}

func TestRulesCheck(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := a.run(t, rulesCmd, "check", writeFile(t, "good.sieve", rulesScript))
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "good.sieve: ok")

	got = a.run(t, rulesCmd, "check", writeFile(t, "bad.sieve", `fileinto "X";`))
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "bad.sieve")

	got = a.run(t, rulesCmd, "check")
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "no rules script")

	got = withRules(t, rulesScript).run(t, rulesCmd, "check")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "rules.sieve: ok")
}

func TestRulesTest(t *testing.T) {
	t.Parallel()

	a := withRules(t, rulesScript)

	got := a.run(t, rulesCmd, "test", writeFile(t, "list.eml", listMessage))
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Lists/Go")

	got = a.run(t, rulesCmd, "test", writeFile(t, "plain.eml", plainMessage))
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "stays")

	got = newAccount(t, "").run(t, rulesCmd, "test", writeFile(t, "plain.eml", plainMessage))
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "no rules script")

	got = a.run(t, rulesCmd, "test", filepath.Join(t.TempDir(), "missing.eml"))
	require.Equal(t, 1, got.code)
}

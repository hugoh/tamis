package cli_test

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const (
	needsArg     = "requires at least 1 arg"
	whitelistCmd = "whitelist"
)

func TestWhitelist(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := a.run(t, whitelistCmd, "list")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No addresses whitelisted")

	got = a.run(t, whitelistCmd, "add", "Bob@Example.com", "alice <alice@example.com>")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "whitelisted bob@example.com")
	require.Contains(t, got.stdout, "whitelisted alice@example.com")

	got = a.run(t, whitelistCmd, "list")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "ADDRESS")
	require.Contains(t, got.stdout, "alice@example.com")
	require.Contains(t, got.stdout, "bob@example.com")
	require.Contains(t, got.stdout, "manual")
	require.Less(t, strings.Index(got.stdout, "alice@"), strings.Index(got.stdout, "bob@"))

	got = a.run(t, whitelistCmd, "rm", "BOB@example.com")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "removed bob@example.com")

	got = a.run(t, whitelistCmd, "list")
	require.NotContains(t, got.stdout, "bob@")
	require.Contains(t, got.stdout, "alice@")
}

func TestWhitelistArguments(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := map[string]struct {
		args []string
		code int
		want string
	}{
		"add without address": {[]string{whitelistCmd, "add"}, 2, needsArg},
		"rm without address":  {[]string{whitelistCmd, "rm"}, 2, needsArg},
		"invalid address":     {[]string{whitelistCmd, "add", "nope"}, 1, `invalid address "nope"`},
		"rm of absent": {
			[]string{whitelistCmd, "rm", "x@example.com"},
			1,
			"x@example.com is not whitelisted",
		},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			got := a.run(t, tt.args...)
			require.Equal(t, tt.code, got.code)
			require.Contains(t, got.stderr, tt.want)
		})
	}
}

func TestWhitelistRemoveMatchingShowsBeforeRemoving(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	require.Zero(
		t,
		a.run(
			t,
			whitelistCmd,
			"add",
			"alice@example.com",
			"bob@example.com",
			"carol@other.net",
		).code,
	)

	got := a.run(t, whitelistCmd, "rm", "--matching", "EXAMPLE.com")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "alice@example.com")
	require.Contains(t, got.stdout, "bob@example.com")
	require.NotContains(t, got.stdout, "carol@other.net")
	require.Contains(t, got.stdout, "2 addresses match")
	require.Contains(t, got.stdout, "--yes")

	left := a.run(t, whitelistCmd, "list")
	require.Contains(t, left.stdout, "alice@example.com", "without --yes nothing is removed")

	got = a.run(t, whitelistCmd, "rm", "--matching", "example.com", "--yes")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "removed 2 addresses")

	left = a.run(t, whitelistCmd, "list")
	require.NotContains(t, left.stdout, "alice@example.com")
	require.Contains(t, left.stdout, "carol@other.net")
}

func TestWhitelistRemoveMatchingWithNothingToRemove(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	require.Zero(t, a.run(t, whitelistCmd, "add", "alice@example.com").code)

	got := a.run(t, whitelistCmd, "rm", "--matching", "zzz", "--yes")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No addresses match")
}

func TestWhitelistRemoveMatchingOne(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	require.Zero(t, a.run(t, whitelistCmd, "add", "alice@example.com").code)

	require.Contains(
		t,
		a.run(t, whitelistCmd, "rm", "--matching", "alice").stdout,
		"1 address matches",
	)
	require.Contains(
		t,
		a.run(t, whitelistCmd, "rm", "--matching", "alice", "--yes").stdout,
		"removed 1 address\n",
	)
}

func TestWhitelistRemoveArgumentsAreExclusive(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	require.Zero(t, a.run(t, whitelistCmd, "add", "alice@example.com").code)

	got := a.run(t, whitelistCmd, "rm", "--matching", "alice", "alice@example.com", "--yes")
	require.NotZero(t, got.code)
	require.Contains(t, got.stderr, "either")

	left := a.run(t, whitelistCmd, "list")
	require.Contains(t, left.stdout, "alice@example.com", "nothing was removed")
}

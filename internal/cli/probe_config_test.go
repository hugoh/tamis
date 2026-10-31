package cli_test

import (
	"context"
	"os"
	"regexp"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestProbeDefaultsToTheConfig(t *testing.T) {
	t.Parallel()

	got := newAccount(t, "").run(t, probeCmd)
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "LIST")
	require.Contains(t, got.stdout, "INBOX")
}

func TestProbeFlagsOverrideTheConfig(t *testing.T) {
	t.Parallel()

	got := newAccount(t, "").run(t, probeCmd, hostFlag, "127.0.0.1", "--port", "1", "--plaintext")
	require.Equal(t, 1, got.code, "the flags win over the configured server")
}

func TestProbePlaintextChecksTheConfiguredHost(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	body, err := os.ReadFile(a.config)
	require.NoError(t, err)

	hostLine := regexp.MustCompile(`(?m)^host = .*$`)
	remote := writeFile(
		t,
		"remote.toml",
		string(hostLine.ReplaceAll(body, []byte(`host = "imap.example.net"`))),
	)

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	got := execute(ctx, t, invocation{args: []string{probeCmd, plaintextFlag, "-c", remote}})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "loopback")
}

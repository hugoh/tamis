package cli_test

import (
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/store"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	webCmd       = "web"
	revokeCmd    = "revoke"
	enrollMarker = "/auth/enroll?token="
)

func (a *account) enableWeb(t *testing.T) {
	t.Helper()

	a.enableWebAt(t, "127.0.0.1:0")
}

func (a *account) enableWebAt(t *testing.T, listen string) {
	t.Helper()

	file, err := os.OpenFile(a.config, os.O_APPEND|os.O_WRONLY, 0)
	require.NoError(t, err)

	_, err = fmt.Fprintf(file,
		"\n[web]\nenabled = true\nlisten = %q\norigin = \"http://localhost:8484\"\n", listen)
	require.NoError(t, err)
	require.NoError(t, file.Close())
}

func (a *account) openStore(t *testing.T) *store.Store {
	t.Helper()

	database, err := store.Open(
		t.Context(),
		filepath.Join(filepath.Dir(a.config), "state", "tamis.db"),
	)
	require.NoError(t, err)
	t.Cleanup(func() { _ = database.Close() })

	return database
}

func TestWebEnroll(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.enableWeb(t)

	got := a.run(t, webCmd, "enroll")
	require.Zero(t, got.code, got.stderr)

	_, token, found := strings.Cut(strings.TrimSpace(got.stdout), enrollMarker)
	require.True(t, found, got.stdout)
	require.Contains(t, got.stdout, "http://localhost:8484"+enrollMarker)
	require.Contains(t, got.stdout, "15 minutes")

	database := a.openStore(t)

	used, err := database.ConsumeEnrollment(t.Context(), web.HashToken(token), time.Now())
	require.NoError(t, err)
	require.True(t, used, "the stored hash matches the printed token")
}

func TestWebNeedsEnabling(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	for _, sub := range []string{"enroll", "passkeys"} {
		got := a.run(t, webCmd, sub)
		require.Equal(t, 1, got.code, sub)
		require.Contains(t, got.stderr, "web is not enabled", sub)
	}
}

func TestWebPasskeys(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.enableWeb(t)

	got := a.run(t, webCmd, "passkeys")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No passkeys enrolled")

	database := a.openStore(t)
	require.NoError(t, database.AddPasskey(t.Context(), "c1", "laptop", []byte("{}")))
	require.NoError(t, database.AddPasskey(t.Context(), "c2", "phone", []byte("{}")))

	got = a.run(t, webCmd, "passkeys")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "laptop")
	require.Contains(t, got.stdout, "phone")
	require.Contains(t, got.stdout, "NAME")

	got = a.run(t, webCmd, revokeCmd, "1")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "revoked passkey 1")

	got = a.run(t, webCmd, "passkeys")
	require.NotContains(t, got.stdout, "laptop")
	require.Contains(t, got.stdout, "phone")
}

func TestWebRevokeArguments(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.enableWeb(t)

	tests := map[string]struct {
		args []string
		code int
		want string
	}{
		"no id":   {[]string{webCmd, revokeCmd}, 2, needsArg},
		"not id":  {[]string{webCmd, revokeCmd, "abc"}, 1, `invalid passkey ID "abc"`},
		"unknown": {[]string{webCmd, revokeCmd, "99"}, 1, "not found"},
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

func TestRunServesTheWebInterface(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.enableWeb(t)

	got := a.run(t, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stderr, "web interface listening")
}

func TestRunWithoutWebStaysQuiet(t *testing.T) {
	t.Parallel()

	got := newAccount(t, "").run(t, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)
	require.NotContains(t, got.stderr, "web interface")
}

func TestRunFailsWhenTheWebAddressIsTaken(t *testing.T) {
	t.Parallel()

	var lc net.ListenConfig

	taken, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)
	t.Cleanup(func() { _ = taken.Close() })

	a := newAccount(t, "")
	a.enableWebAt(t, taken.Addr().String())

	got := a.run(t, runCmd, onceFlag)
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "address already in use")
	require.NotContains(t, got.stderr, "pass complete", "it fails before touching the mailbox")
}

func TestDoctorReportsTheWebInterface(t *testing.T) {
	t.Parallel()

	off := newAccount(t, "").run(t, "doctor")
	require.Contains(t, off.stdout, "web interface")
	require.Contains(t, off.stdout, "not enabled")
	require.NotContains(t, off.stdout, "web passkeys")

	a := newAccount(t, "")
	a.enableWeb(t)

	got := a.run(t, "doctor")
	require.Contains(t, got.stdout, "WARN")
	require.Contains(
		t,
		got.stdout,
		"nothing is listening on 127.0.0.1:0",
		"nothing is running in a test",
	)
	require.Contains(t, got.stdout, "none enrolled; run 'tamis web enroll'")

	require.NoError(t, a.openStore(t).AddPasskey(t.Context(), "c1", "laptop", []byte("{}")))

	got = a.run(t, "doctor", "--json")
	require.Contains(t, got.stdout, `"name": "web passkeys"`)
	require.Contains(t, got.stdout, "1 enrolled")
}

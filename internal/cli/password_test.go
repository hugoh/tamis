package cli_test

import (
	"errors"
	"net"
	"os"
	"path/filepath"
	"testing"

	"github.com/hugoh/tamis/internal/cli"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/secret"
	"github.com/hugoh/tamis/internal/secret/secrettest"
	"github.com/stretchr/testify/require"
)

const loopbackService = "tamis/127.0.0.1"

// brokenKeyring is a keychain that can't be reached, as on a headless server.
type brokenKeyring struct{}

var errNoKeychain = errors.New("no keychain on this machine")

func (brokenKeyring) Get(string, string) (string, error) { return "", errNoKeychain }
func (brokenKeyring) Set(string, string, string) error   { return errNoKeychain }
func (brokenKeyring) Delete(string, string) error        { return errNoKeychain }

func answer(password string) func(string) (string, error) {
	return func(string) (string, error) { return password, nil }
}

func TestLoginLogout(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	ring := secrettest.NewMemory()

	got := execute(t.Context(), t, invocation{
		args: []string{loginCmd, configFlag, a.config}, srv: a.srv, keyring: ring,
		prompt: answer(imaptest.Password),
	})
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Saved")

	stored, err := ring.Get(loopbackService, imaptest.Username)
	require.NoError(t, err)
	require.Equal(t, imaptest.Password, stored)

	got = execute(t.Context(), t, invocation{
		args: []string{runCmd, onceFlag, configFlag, a.config}, srv: a.srv, keyring: ring,
	})
	require.Zero(t, got.code, "the keychain is enough: %s", got.stderr)

	got = execute(t.Context(), t, invocation{
		args: []string{logoutCmd, configFlag, a.config}, srv: a.srv, keyring: ring,
	})
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Removed")

	got = execute(t.Context(), t, invocation{
		args: []string{logoutCmd, configFlag, a.config}, srv: a.srv, keyring: ring,
	})
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No password stored")

	got = execute(t.Context(), t, invocation{
		args: []string{runCmd, onceFlag, configFlag, a.config}, srv: a.srv, keyring: ring,
	})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "tamis login")
}

func TestLoginFailures(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := []struct {
		name    string
		prompt  func(string) (string, error)
		keyring secret.Keyring
		want    string
	}{
		{"wrong password", answer("wrong"), secrettest.NewMemory(), loginFailed},
		{"no keychain", answer(imaptest.Password), brokenKeyring{}, "password_command"},
		{"no keychain support at all", answer(imaptest.Password), nil, "password_command"},
		{"empty password", answer(""), secrettest.NewMemory(), noPassword},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got := execute(t.Context(), t, invocation{
				args: []string{loginCmd, configFlag, a.config}, srv: a.srv,
				keyring: tt.keyring, prompt: tt.prompt,
			})
			require.Equal(t, 1, got.code)
			require.Contains(t, got.stderr, tt.want)
		})
	}
}

func TestLoginFromStdin(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	ring := secrettest.NewMemory()

	got := execute(t.Context(), t, invocation{
		args: []string{loginCmd, configFlag, a.config}, srv: a.srv, keyring: ring,
		prompt: func(string) (string, error) { return "", cli.ErrNotTerminal },
		stdin:  imaptest.Password + "\n",
	})
	require.Zero(t, got.code, got.stderr)

	stored, err := ring.Get(loopbackService, imaptest.Username)
	require.NoError(t, err)
	require.Equal(t, imaptest.Password, stored)
}

func TestLogoutWithoutKeychain(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := execute(t.Context(), t, invocation{
		args: []string{logoutCmd, configFlag, a.config}, srv: a.srv, keyring: brokenKeyring{},
	})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "no keychain")
}

func TestPasswordSources(t *testing.T) {
	t.Parallel()

	stored := func(password string) secret.Keyring {
		ring := secrettest.NewMemory()
		_ = ring.Set(loopbackService, imaptest.Username, password)

		return ring
	}

	tests := []struct {
		name       string
		credential string
		keyring    secret.Keyring
		env        string
	}{
		{"systemd credential beats the keychain", imaptest.Password, stored("wrong"), ""},
		{"keychain beats the environment", "", stored(imaptest.Password), "wrong"},
		{"environment when the keychain is empty", "", secrettest.NewMemory(), imaptest.Password},
		{"environment when the keychain is broken", "", brokenKeyring{}, imaptest.Password},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			a := newAccount(t, "")
			env := map[string]string{}

			if tt.credential != "" {
				dir := t.TempDir()
				require.NoError(
					t,
					os.WriteFile(filepath.Join(dir, "password"), []byte(tt.credential+"\n"), 0o600),
				)
				env["CREDENTIALS_DIRECTORY"] = dir
			}

			if tt.env != "" {
				env[passwordKey] = tt.env
			}

			got := execute(t.Context(), t, invocation{
				args: []string{runCmd, onceFlag, configFlag, a.config}, srv: a.srv,
				keyring: tt.keyring, env: env,
			})
			require.Zero(t, got.code, got.stderr)
		})
	}
}

func TestProbeUsesKeychain(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	host, _, err := net.SplitHostPort(srv.Addr)
	require.NoError(t, err)

	ring := secrettest.NewMemory()
	require.NoError(t, ring.Set(secret.Service(host), imaptest.Username, imaptest.Password))

	got := execute(t.Context(), t, invocation{args: probeArgs(t, srv), keyring: ring})
	require.Zero(t, got.code, got.stderr)
}

func TestUnreadableSystemdCredential(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	dir := t.TempDir()
	require.NoError(t, os.Mkdir(filepath.Join(dir, "password"), 0o700))

	got := execute(t.Context(), t, invocation{
		args: []string{runCmd, onceFlag, configFlag, a.config}, srv: a.srv,
		env: map[string]string{"CREDENTIALS_DIRECTORY": dir},
	})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "systemd credential")
}

package cli_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/cli"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/secret"
	"github.com/stretchr/testify/require"
)

const (
	probeCmd      = "probe"
	userFlag      = "--user"
	configFlag    = "--config"
	runCmd        = "run"
	passwordKey   = "TAMIS_PASSWORD"
	missingConfig = "/nonexistent/config.toml"
	onceFlag      = "--once"
	hostFlag      = "--host"
	loginCmd      = "login"
	logoutCmd     = "logout"
	noPassword    = "no password"
	loginFailed   = "login failed"
)

type result struct {
	code           int
	stdout, stderr string
}

type invocation struct {
	args   []string
	stdin  string
	env    map[string]string
	srv    *imaptest.Server
	prompt func(string) (string, error)
	// keyring stands in for the system keychain; nil means none is
	// available. Tests never touch the real one.
	keyring secret.Keyring
}

func execute(ctx context.Context, t *testing.T, inv invocation) result {
	t.Helper()

	var stdout, stderr bytes.Buffer

	env := cli.Env{
		Stdin:   strings.NewReader(inv.stdin),
		Stdout:  &stdout,
		Stderr:  &stderr,
		Getenv:  func(key string) string { return inv.env[key] },
		Prompt:  inv.prompt,
		Keyring: inv.keyring,
	}

	if inv.srv != nil {
		env.TLSConfig = inv.srv.TLSConfig
	}

	code := cli.Run(ctx, inv.args, env, "1.2.3")

	return result{code: code, stdout: stdout.String(), stderr: stderr.String()}
}

func TestRun(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name       string
		args       []string
		wantCode   int
		wantStdout string
		wantStderr string
	}{
		{name: "no args shows help", args: nil, wantStdout: "Available Commands:"},
		{name: "version", args: []string{"version"}, wantStdout: "tamis 1.2.3\n"},
		{
			name:       "config shorthand",
			args:       []string{"status", "-c", missingConfig},
			wantCode:   1,
			wantStderr: missingConfig,
		},
		{name: "version flag", args: []string{"--version"}, wantStdout: "tamis 1.2.3\n"},
		{name: "help", args: []string{"help"}, wantStdout: "probe"},
		{
			name:       "unknown command",
			args:       []string{"bogus"},
			wantCode:   1,
			wantStderr: `unknown command "bogus"`,
		},
		{name: "probe help", args: []string{probeCmd, "-h"}, wantStdout: passwordKey},
		{
			name:       "probe bad flag",
			args:       []string{probeCmd, "--nope"},
			wantCode:   1,
			wantStderr: "unknown flag",
		},
		{
			name:       "probe without user",
			args:       []string{probeCmd},
			wantCode:   1,
			wantStderr: `"user" not set`,
		},
		{
			name:       "probe without host",
			args:       []string{probeCmd, userFlag, "me"},
			wantCode:   1,
			wantStderr: `"host" not set`,
		},
		{
			name:       "probe without password",
			args:       []string{probeCmd, hostFlag, "imap.example.com", userFlag, "me"},
			wantCode:   1,
			wantStderr: noPassword,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got := execute(t.Context(), t, invocation{args: tt.args})
			require.Equal(t, tt.wantCode, got.code, "stderr: %s", got.stderr)
			require.Contains(t, got.stdout, tt.wantStdout)
			require.Contains(t, got.stderr, tt.wantStderr)
		})
	}
}

func probeArgs(t *testing.T, srv *imaptest.Server, extra ...string) []string {
	t.Helper()

	host, port, err := net.SplitHostPort(srv.Addr)
	require.NoError(t, err)

	return append([]string{
		probeCmd,
		"--plaintext",
		"--idle-wait",
		"5s",
		hostFlag,
		host,
		"--port",
		port,
		userFlag,
		imaptest.Username,
	}, extra...)
}

func TestProbeText(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	got := execute(
		t.Context(),
		t,
		invocation{args: probeArgs(t, srv, "--write"), stdin: imaptest.Password + "\n"},
	)
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "native MOVE")
}

func TestProbeJSON(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	got := execute(t.Context(), t, invocation{
		args: probeArgs(t, srv, "--json"),
		env:  map[string]string{passwordKey: imaptest.Password},
	})
	require.Zero(t, got.code, got.stderr)

	var report struct {
		Checks []struct {
			Name string `json:"name"`
		} `json:"checks"`
	}

	require.NoError(t, json.Unmarshal([]byte(got.stdout), &report))
	require.NotEmpty(t, report.Checks)
}

func TestProbeLoginFailure(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	got := execute(t.Context(), t, invocation{args: probeArgs(t, srv), stdin: "wrong\n"})
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, loginFailed)
}

func TestProbeFailedChecks(t *testing.T) {
	t.Parallel()

	// A bare IMAP4rev1 server lacks UIDPLUS, which the probe requires.
	srv := imaptest.Start(t, imaptest.Rev1())

	got := execute(
		t.Context(),
		t,
		invocation{args: probeArgs(t, srv), stdin: imaptest.Password + "\n"},
	)
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stdout, "FAIL")
}

// errReader fails every read.
type errReader struct{}

func (errReader) Read([]byte) (int, error) { return 0, errors.New("stdin closed") }

func TestProbePasswordReadError(t *testing.T) {
	t.Parallel()

	var stderr bytes.Buffer

	code := cli.Run(
		t.Context(),
		[]string{probeCmd, hostFlag, "imap.example.com", userFlag, "me"},
		cli.Env{
			Stdin:  errReader{},
			Stdout: &bytes.Buffer{},
			Stderr: &stderr,
			Getenv: func(string) string { return "" },
		},
		"1.2.3",
	)
	require.Equal(t, 1, code)
	require.Contains(t, stderr.String(), "read password")
}

func TestProbePassword(t *testing.T) {
	t.Parallel()

	errBroken := errors.New("terminal broke")

	tests := []struct {
		name     string
		prompt   func(string) (string, error)
		stdin    string
		wantCode int
		want     string
	}{
		{
			name:   "prompted on a terminal",
			prompt: func(string) (string, error) { return imaptest.Password, nil },
		},
		{
			name:   "stdin when not a terminal",
			prompt: func(string) (string, error) { return "", cli.ErrNotTerminal },
			stdin:  imaptest.Password + "\n",
		},
		{
			name:     "prompt fails",
			prompt:   func(string) (string, error) { return "", errBroken },
			wantCode: 1,
			want:     "terminal broke",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())

			var asked string

			got := execute(t.Context(), t, invocation{
				args:  probeArgs(t, srv),
				stdin: tt.stdin,
				prompt: func(text string) (string, error) {
					asked = text

					return tt.prompt(text)
				},
			})
			require.Equal(t, tt.wantCode, got.code, got.stderr)
			require.Contains(t, got.stderr, tt.want)
			require.Contains(t, asked, imaptest.Username, "the prompt names the account")
		})
	}
}

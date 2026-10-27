package cli_test

import (
	"context"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/require"
)

const seenFlag = `\Seen`

type account struct {
	srv    *imaptest.Server
	config string
	client *mail.Client
	serial int
}

func passwordEnv() map[string]string {
	return map[string]string{passwordKey: imaptest.Password}
}

func newAccount(t *testing.T, policy string) *account {
	t.Helper()

	srv := imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true})

	host, port, err := net.SplitHostPort(srv.Addr)
	require.NoError(t, err)

	dir := t.TempDir()
	path := filepath.Join(dir, "config.toml")
	body := fmt.Sprintf(`state = %q

[account]
host = %q
port = %s
user = %q

[policy]
%s
`, filepath.Join(dir, "state", "tamis.db"), host, port, imaptest.Username, policy)
	require.NoError(t, os.WriteFile(path, []byte(body), 0o600))

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr:      srv.Addr,
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		TLSConfig: srv.TLSConfig,
	})
	require.NoError(t, err)
	t.Cleanup(func() { _ = client.Close() })

	return &account{srv: srv, config: path, client: client}
}

func (a *account) run(t *testing.T, args ...string) result {
	t.Helper()

	return a.runContext(t.Context(), t, args...)
}

func (a *account) runContext(ctx context.Context, t *testing.T, args ...string) result {
	t.Helper()

	return execute(ctx, t, invocation{
		args: append(args, configFlag, a.config),
		env:  passwordEnv(),
		srv:  a.srv,
	})
}

func (a *account) add(
	t *testing.T,
	mailbox string,
	flags []string,
	date time.Time,
	subject, body string,
) {
	t.Helper()

	a.serial++

	require.NoError(t, a.client.Ensure(mailbox))

	raw := fmt.Appendf(nil,
		"From: sender%d@example.net\r\nTo: %s\r\nSubject: %s\r\nMessage-ID: <c%d@test>\r\n"+
			"Content-Type: text/plain\r\n\r\n%s\r\n",
		a.serial, imaptest.Username, subject, a.serial, body)

	_, err := a.client.Append(mailbox, raw, flags, date)
	require.NoError(t, err)
}

func (a *account) seedCorpus(t *testing.T) {
	t.Helper()

	old := time.Now().AddDate(0, 0, -30)

	for range 5 {
		a.add(
			t,
			"Junk",
			nil,
			old,
			"WINNER claim prize",
			"Congratulations winner, claim your lottery prize.",
		)
		a.add(
			t,
			"INBOX",
			[]string{seenFlag},
			old,
			"Project meeting",
			"Agenda for the project meeting.",
		)
	}

	a.add(t, "Sent Messages", []string{seenFlag}, old, "hi", "hello")
}

func TestBootstrapRunLogStatus(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.seedCorpus(t)

	got := a.run(t, "bootstrap")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Trained 5 spam and 5 good messages")

	a.add(
		t,
		"INBOX",
		nil,
		time.Now(),
		"WINNER claim prize",
		"Congratulations winner, claim your lottery prize.",
	)

	got = a.run(t, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stderr, "verdict=spam")

	inbox, err := a.client.UIDsAfter("INBOX", 0)
	require.NoError(t, err)
	require.Len(t, inbox, 6, "shadow mode leaves the spam in place")

	got = a.run(t, "log")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "spam")
	require.Contains(t, got.stdout, "would move:Junk")
	require.Contains(t, got.stdout, "WINNER claim prize")

	got = a.run(t, "status")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "shadow")
	require.Contains(t, got.stdout, "5 spam, 5 good")
}

func TestRunLiveMovesSpam(t *testing.T) {
	t.Parallel()

	a := newAccount(t, `mode = "live"`)
	a.seedCorpus(t)
	require.Zero(t, a.run(t, "bootstrap").code)

	a.add(
		t,
		"INBOX",
		nil,
		time.Now(),
		"WINNER claim prize",
		"Congratulations winner, claim your lottery prize.",
	)

	got := a.run(t, runCmd, onceFlag)
	require.Zero(t, got.code, got.stderr)

	junk, err := a.client.UIDsAfter("Junk", 0)
	require.NoError(t, err)
	require.Len(t, junk, 6)

	got = a.run(t, "log")
	require.Contains(t, got.stdout, " move:Junk")
	require.NotContains(t, got.stdout, "would")
}

func TestRunLoopStopsOnCancel(t *testing.T) {
	t.Parallel()

	a := newAccount(t, `poll_interval = "1h"`)

	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
	defer cancel()

	start := time.Now()
	got := a.runContext(ctx, t, runCmd)
	require.Zero(t, got.code, got.stderr)
	require.Less(t, time.Since(start), time.Minute)
	require.Contains(t, got.stderr, "pass complete")
}

func TestRunLoopSurvivesFailedPass(t *testing.T) {
	t.Parallel()

	a := newAccount(t, `poll_interval = "50ms"`)

	ctx, cancel := context.WithTimeout(t.Context(), 300*time.Millisecond)
	defer cancel()

	got := execute(ctx, t, invocation{
		args: []string{runCmd, configFlag, a.config},
		env:  map[string]string{passwordKey: "wrong"},
		srv:  a.srv,
	})
	require.Zero(t, got.code, "the daemon keeps retrying; stderr: %s", got.stderr)
	require.Contains(t, got.stderr, "pass failed")
}

func TestCommandErrors(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := []struct {
		name string
		args []string
		env  map[string]string
		want string
	}{
		{
			"missing config",
			[]string{"status", configFlag, missingConfig},
			passwordEnv(),
			"read config",
		},
		{
			"run without password",
			[]string{runCmd, onceFlag, configFlag, a.config},
			nil,
			"no password",
		},
		{
			"bootstrap with wrong password",
			[]string{"bootstrap", configFlag, a.config},
			map[string]string{passwordKey: "x"},
			"login failed",
		},
		{
			"run once with wrong password",
			[]string{runCmd, onceFlag, configFlag, a.config},
			map[string]string{passwordKey: "x"},
			"login failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got := execute(t.Context(), t, invocation{args: tt.args, env: tt.env, srv: a.srv})
			require.Equal(t, 1, got.code)
			require.Contains(t, got.stderr, tt.want)
		})
	}
}

func TestLogEmpty(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := a.run(t, "log")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "No decisions yet")
}

func TestBootstrapSources(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.seedCorpus(t)

	old := time.Now().AddDate(0, 0, -30)
	for range 3 {
		a.add(t, "Archive", []string{seenFlag}, old, "Old thread", "Notes from the old thread.")
	}

	got := a.run(t, "bootstrap", "-s", "Junk=2", "--good", "INBOX=1", "-g", "Archive=3")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Trained 2 spam and 4 good messages")
}

func TestBootstrapBadSource(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := map[string]string{
		"Archive":      "invalid argument",
		"Archive=many": "invalid argument",
		"Archive=0":    "count",
		"Archive=-1":   "count",
		"=5":           "folder",
	}

	for value, want := range tests {
		t.Run(value, func(t *testing.T) {
			t.Parallel()

			got := a.run(t, "bootstrap", "--good", value)
			require.Equal(t, 1, got.code)
			require.Contains(t, got.stderr, want)
		})
	}
}

func TestRunPromptsForPassword(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := execute(t.Context(), t, invocation{
		args:   []string{runCmd, onceFlag, configFlag, a.config},
		srv:    a.srv,
		prompt: func(string) (string, error) { return imaptest.Password, nil },
	})
	require.Zero(t, got.code, got.stderr)
}

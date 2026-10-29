package cli_test

import (
	"context"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
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

func TestRunLoopWakesOnNewMail(t *testing.T) {
	t.Parallel()

	a := newAccount(t, `poll_interval = "1h"`)

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	time.AfterFunc(time.Second, func() {
		a.add(t, "INBOX", nil, time.Now(), "Lunch", "Are we still on for lunch?")
	})

	got := a.runContext(ctx, t, runCmd)
	require.Zero(t, got.code, got.stderr)
	require.Equal(t, 2, strings.Count(got.stderr, "pass complete"),
		"new mail should start a pass long before poll_interval")
}

func TestRunLoopStopsOnRejectedLogin(t *testing.T) {
	t.Parallel()

	a := newAccount(t, `poll_interval = "50ms"`)

	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()

	got := execute(ctx, t, invocation{
		args: []string{runCmd, configFlag, a.config},
		env:  map[string]string{passwordKey: "wrong"},
		srv:  a.srv,
	})
	require.Equal(t, 1, got.code, "retrying a rejected login risks locking the account")
	require.Contains(t, got.stderr, loginFailed)
	require.NoError(t, ctx.Err(), "the daemon should exit on its own")
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
			[]string{statusCmd, configFlag, missingConfig},
			passwordEnv(),
			readConfigFailed,
		},
		{
			"doctor with missing config",
			[]string{"doctor", configFlag, missingConfig},
			passwordEnv(),
			readConfigFailed,
		},
		{
			"login with missing config",
			[]string{loginCmd, configFlag, missingConfig},
			passwordEnv(),
			readConfigFailed,
		},
		{
			"logout with missing config",
			[]string{logoutCmd, configFlag, missingConfig},
			passwordEnv(),
			readConfigFailed,
		},
		{
			"run without password",
			[]string{runCmd, onceFlag, configFlag, a.config},
			nil,
			noPassword,
		},
		{
			"bootstrap with wrong password",
			[]string{"bootstrap", configFlag, a.config},
			map[string]string{passwordKey: "x"},
			loginFailed,
		},
		{
			"run once with wrong password",
			[]string{runCmd, onceFlag, configFlag, a.config},
			map[string]string{passwordKey: "x"},
			loginFailed,
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

func TestBootstrapOnlyTheNamedFolders(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
	a.seedCorpus(t)

	got := a.run(t, "bootstrap", "-g", "INBOX=5")
	require.Zero(t, got.code, got.stderr)
	require.Equal(t, "Trained 5 good messages.\n", got.stdout)

	got = a.run(t, "status")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "0 spam, 5 good")
	require.Contains(t, got.stdout, "0 addresses")
}

func TestBootstrapBadSource(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := map[string]struct {
		code int
		want string
	}{
		"Archive":      {2, "invalid argument"},
		"Archive=many": {2, "invalid argument"},
		"Archive=0":    {1, "count"},
		"Archive=-1":   {1, "count"},
		"=5":           {1, "folder"},
	}

	for value, tt := range tests {
		t.Run(value, func(t *testing.T) {
			t.Parallel()

			got := a.run(t, "bootstrap", "--good", value)
			require.Equal(t, tt.code, got.code)
			require.Contains(t, got.stderr, tt.want)
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

func TestUndo(t *testing.T) {
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
	require.Zero(t, a.run(t, runCmd, onceFlag).code)

	got := a.run(t, "log")
	require.Contains(t, got.stdout, "ID")
	require.Contains(t, got.stdout, "move:Junk")

	got = a.run(t, "undo", "1")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "1: moved back to INBOX")

	got = a.run(t, "log")
	require.Contains(t, got.stdout, "move:Junk (undone)")

	inbox, err := a.client.UIDsAfter("INBOX", 0)
	require.NoError(t, err)
	require.Len(t, inbox, 6)

	got = a.run(t, "undo", "1", "42")
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "1: ")
	require.Contains(t, got.stderr, "already undone")
	require.Contains(t, got.stderr, "42: ")
	require.Contains(t, got.stderr, "not found")
}

func TestUndoArguments(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	tests := map[string]struct {
		args []string
		code int
		want string
	}{
		"no id":  {[]string{"undo"}, 2, "requires at least 1 arg"},
		"not id": {[]string{"undo", "abc"}, 1, `invalid decision ID "abc"`},
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

func TestLearn(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")
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
	require.Zero(t, a.run(t, runCmd, onceFlag).code)

	got := a.run(t, "learn", "1")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "1: learnt as good")

	got = a.run(t, "log")
	require.Contains(t, got.stdout, "(corrected by 2)")

	got = a.run(t, "learn", "1")
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "already corrected by decision 2")

	got = a.run(t, "learn", "--spam", "1")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "1: learnt as spam")

	got = a.run(t, "learn", "--good", "--spam", "1")
	require.Equal(t, 2, got.code)
	require.Contains(t, got.stderr, "none of the others can be")

	got = a.run(t, "learn", "42")
	require.Equal(t, 1, got.code)
	require.Contains(t, got.stderr, "42: ")
	require.Contains(t, got.stderr, "not found")
}

func TestDoctor(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := a.run(t, "doctor")
	require.Equal(t, 1, got.code, "an untrained model is a failure")
	require.Contains(t, got.stdout, "FAIL")
	require.Contains(t, got.stdout, "class balance")

	a.seedCorpus(t)
	require.Zero(t, a.run(t, "bootstrap").code)

	got = a.run(t, "doctor")
	require.Zero(t, got.code, got.stdout)
	require.Contains(t, got.stdout, "WARN")
	require.Contains(t, got.stdout, "5 spam, 5 good")

	got = a.run(t, "doctor", "--json")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, `"name": "class balance"`)
}

func TestStatusShowsAuthServerID(t *testing.T) {
	t.Parallel()

	a := newAccount(t, "")

	got := a.run(t, "status")
	require.Zero(t, got.code, got.stderr)
	require.Contains(t, got.stdout, "Auth server id:")
	require.Contains(t, got.stdout, "not set")
}

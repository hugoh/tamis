package cli_test

import (
	"bytes"
	"encoding/json"
	"errors"
	"net"
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/cli"
	"github.com/hugoh/tamis/internal/imaptest"
)

type result struct {
	code           int
	stdout, stderr string
}

func run(args []string, stdin string, env map[string]string) result {
	var stdout, stderr bytes.Buffer

	code := cli.Run(args, cli.Env{
		Stdin:  strings.NewReader(stdin),
		Stdout: &stdout,
		Stderr: &stderr,
		Getenv: func(key string) string { return env[key] },
	}, "1.2.3")

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
		{name: "no args", args: nil, wantCode: 2, wantStderr: "Usage:"},
		{name: "version", args: []string{"version"}, wantCode: 0, wantStdout: "tamis 1.2.3\n"},
		{
			name:       "version flag",
			args:       []string{"--version"},
			wantCode:   0,
			wantStdout: "tamis 1.2.3\n",
		},
		{name: "help", args: []string{"help"}, wantCode: 0, wantStdout: "Commands:"},
		{
			name:       "unknown",
			args:       []string{"bogus"},
			wantCode:   2,
			wantStderr: `unknown command "bogus"`,
		},
		{
			name:       "probe help",
			args:       []string{probeCmd, "-h"},
			wantCode:   0,
			wantStderr: "TAMIS_PASSWORD",
		},
		{
			name:       "probe bad flag",
			args:       []string{probeCmd, "--nope"},
			wantCode:   2,
			wantStderr: "not defined",
		},
		{
			name:       "probe no user",
			args:       []string{probeCmd},
			wantCode:   2,
			wantStderr: "--user is required",
		},
		{
			name:       "probe no password",
			args:       []string{probeCmd, userFlag, "me"},
			wantCode:   2,
			wantStderr: "no password",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got := run(tt.args, "", nil)
			if got.code != tt.wantCode {
				t.Errorf("exit code = %d, want %d", got.code, tt.wantCode)
			}

			assertContains(t, "stdout", got.stdout, tt.wantStdout)
			assertContains(t, "stderr", got.stderr, tt.wantStderr)
		})
	}
}

const (
	probeCmd = "probe"
	userFlag = "--user"
)

func probeArgs(t *testing.T, extra ...string) []string {
	t.Helper()

	return probeArgsFor(t, imaptest.Start(t, imaptest.Rev2()), extra...)
}

func probeArgsFor(t *testing.T, srv *imaptest.Server, extra ...string) []string {
	t.Helper()

	host, port, err := net.SplitHostPort(srv.Addr)
	if err != nil {
		t.Fatal(err)
	}

	return append([]string{
		probeCmd, "--plaintext", "--idle-wait", "5s",
		"--host", host, "--port", port, userFlag, imaptest.Username,
	}, extra...)
}

func TestProbeText(t *testing.T) {
	t.Parallel()

	got := run(probeArgs(t, "--write"), imaptest.Password+"\n", nil)
	if got.code != 0 {
		t.Fatalf("exit code = %d, stderr %q, stdout:\n%s", got.code, got.stderr, got.stdout)
	}

	assertContains(t, "stdout", got.stdout, "native MOVE")
}

func TestProbeJSON(t *testing.T) {
	t.Parallel()

	env := map[string]string{"TAMIS_PASSWORD": imaptest.Password}

	got := run(probeArgs(t, "--json"), "", env)
	if got.code != 0 {
		t.Fatalf("exit code = %d, stderr %q", got.code, got.stderr)
	}

	var report struct {
		Checks []struct {
			Name string `json:"name"`
		} `json:"checks"`
	}

	if err := json.Unmarshal([]byte(got.stdout), &report); err != nil {
		t.Fatalf("decode %q: %v", got.stdout, err)
	}

	if len(report.Checks) == 0 {
		t.Error("no checks in JSON report")
	}
}

func TestProbeLoginFailure(t *testing.T) {
	t.Parallel()

	got := run(probeArgs(t), "wrong\n", nil)
	if got.code != 1 {
		t.Errorf("exit code = %d, want 1", got.code)
	}

	assertContains(t, "stderr", got.stderr, "login failed")
}

func TestProbeFailedChecks(t *testing.T) {
	t.Parallel()

	// A bare IMAP4rev1 server lacks UIDPLUS, which the probe requires.
	args := probeArgsFor(t, imaptest.Start(t, imaptest.Rev1()))

	got := run(args, imaptest.Password+"\n", nil)
	if got.code != 1 {
		t.Errorf("exit code = %d, want 1", got.code)
	}

	assertContains(t, "stdout", got.stdout, "FAIL")
}

// errReader fails every read.
type errReader struct{}

func (errReader) Read([]byte) (int, error) { return 0, errors.New("stdin closed") }

func TestProbePasswordReadError(t *testing.T) {
	t.Parallel()

	var stdout, stderr bytes.Buffer

	code := cli.Run([]string{probeCmd, userFlag, "me"}, cli.Env{
		Stdin:  errReader{},
		Stdout: &stdout,
		Stderr: &stderr,
		Getenv: func(string) string { return "" },
	}, "1.2.3")
	if code != 2 {
		t.Errorf("exit code = %d, want 2", code)
	}

	assertContains(t, "stderr", stderr.String(), "read password")
}

func assertContains(t *testing.T, stream, got, want string) {
	t.Helper()

	if want == "" {
		return
	}

	if !strings.Contains(got, want) {
		t.Errorf("%s = %q, want it to contain %q", stream, got, want)
	}
}

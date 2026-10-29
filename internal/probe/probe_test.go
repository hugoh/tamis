package probe

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"net"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/stretchr/testify/require"
)

const (
	opMove   = "MOVE"
	opSelect = "SELECT"
)

func config(srv *imaptest.Server) Config {
	return Config{
		Addr:      srv.Addr,
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		IdleWait:  5 * time.Second,
		Plaintext: srv.TLSConfig == nil,
		TLSConfig: srv.TLSConfig,
		Timeout:   5 * time.Second,
	}
}

func find(t *testing.T, r *Report, name string) Check {
	t.Helper()

	for _, c := range r.Checks {
		if c.Name == name {
			return c
		}
	}

	t.Fatalf("no check %q in %+v", name, r.Checks)

	return Check{}
}

func expect(t *testing.T, r *Report, name string, status Status, detail string) {
	t.Helper()

	c := find(t, r, name)
	require.Equal(t, status, c.Status, "%s: %s", name, c.Detail)
	require.Contains(t, c.Detail, detail, name)
}

func TestRunReadOnly(t *testing.T) {
	t.Parallel()

	report, err := Run(config(imaptest.Start(t, imaptest.Rev2())))
	require.NoError(t, err)

	require.False(t, report.Failed(), "%+v", report.Checks)

	expect(t, report, "login", StatusOK, imaptest.Username)
	expect(t, report, "IDLE", StatusOK, "advertised")
	expect(t, report, `folder \Junk`, StatusWarn, `guessed "Junk"`)
	expect(t, report, `folder \Sent`, StatusWarn, "not found")
	expect(t, report, "EXAMINE INBOX", StatusOK, "0 messages")
	expect(t, report, "write checks", StatusSkip, "--write")

	require.Len(t, report.Mailboxes, 2, "want INBOX and Junk")
}

func TestRunWrite(t *testing.T) {
	t.Parallel()

	cfg := config(imaptest.Start(t, imaptest.Rev2()))
	cfg.Write = true

	report, err := Run(cfg)
	require.NoError(t, err)

	require.False(t, report.Failed(), "%+v", report.Checks)

	expect(t, report, "CREATE scratch", StatusOK, "tamis-probe-")
	expect(t, report, "APPEND", StatusOK, "APPENDUID")
	expect(t, report, "BODY.PEEK", StatusOK, "unread")
	expect(t, report, "STORE keyword", StatusOK, "$TamisProbe")
	expect(t, report, "second connection", StatusOK, "two")
	expect(t, report, "IDLE push", StatusOK, "pushed after")
	expect(t, report, "MOVE message", StatusOK, "native MOVE")
	expect(t, report, "DELETE scratch", StatusOK, "removed")
}

func TestRunWriteRev1(t *testing.T) {
	t.Parallel()

	cfg := config(imaptest.Start(t, imaptest.Rev1()))
	cfg.Write = true

	report, err := Run(cfg)
	require.NoError(t, err)

	require.True(t, report.Failed(), "missing UIDPLUS should fail the report")

	expect(t, report, "UIDPLUS", StatusFail, "missing")
	expect(t, report, opMove, StatusWarn, "missing")
	expect(t, report, "MOVE message", StatusOK, "unsafe outside scratch")
	expect(t, report, "DELETE scratch", StatusOK, "removed")
}

func TestRunTLS(t *testing.T) {
	t.Parallel()

	report, err := Run(
		config(imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true})),
	)
	require.NoError(t, err)

	expect(t, report, "login", StatusOK, "")
}

func TestRunBadPassword(t *testing.T) {
	t.Parallel()

	cfg := config(imaptest.Start(t, imaptest.Rev2()))
	cfg.Password = "wrong"

	_, err := Run(cfg)
	require.ErrorIs(t, err, ErrLogin)
}

func TestRunConnectRefused(t *testing.T) {
	t.Parallel()

	ln, err := (&net.ListenConfig{}).Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)

	addr := ln.Addr().String()
	_ = ln.Close()

	_, err = Run(Config{Addr: addr, Plaintext: true})
	require.Error(t, err, "dialing a closed port")
}

func TestWriteReport(t *testing.T) {
	t.Parallel()

	report := &Report{
		Server:       "imap.example.com:993",
		Capabilities: []string{"IDLE", opMove},
		Mailboxes:    []Mailbox{{Name: "Junk", Attrs: []string{`\Junk`}}},
		Checks: []Check{
			{Name: opMove, Status: StatusOK, Detail: "native MOVE"},
			{Name: "CONDSTORE", Status: StatusWarn, Detail: "missing"},
		},
	}

	var text bytes.Buffer
	require.NoError(t, report.WriteText(&text))

	for _, want := range []string{"imap.example.com:993", "IDLE MOVE", "native MOVE", `\Junk`, "WARN"} {
		require.Contains(t, text.String(), want)
	}

	var buf bytes.Buffer
	require.NoError(t, report.WriteJSON(&buf))

	var decoded Report
	require.NoError(t, json.Unmarshal(buf.Bytes(), &decoded))

	require.Equal(t, StatusOK, decoded.Checks[0].Status)
}

// secondScratchSelect fails the IDLE watcher's SELECT, the second one on the
// scratch folder.
func secondScratchSelect() func(op, mailbox string) bool {
	var selects atomic.Int32

	return func(op, mailbox string) bool {
		return op == opSelect && strings.HasPrefix(mailbox, "tamis-probe-") && selects.Add(1) == 2
	}
}

func TestRunInjectedFailures(t *testing.T) {
	t.Parallel()

	scratch := func(mailbox string) bool { return strings.HasPrefix(mailbox, "tamis-probe-") }

	tests := []struct {
		name   string
		fail   func(op, mailbox string) bool
		check  string
		status Status
	}{
		{"list", func(op, _ string) bool { return op == "LIST" }, "LIST", StatusFail},
		{
			"examine",
			func(op, m string) bool { return op == opSelect && m == "INBOX" },
			"EXAMINE INBOX",
			StatusFail,
		},
		{"create", func(op, _ string) bool { return op == "CREATE" }, "CREATE scratch", StatusFail},
		{"append", func(op, _ string) bool { return op == "APPEND" }, "APPEND", StatusFail},
		{
			"select",
			func(op, m string) bool { return op == opSelect && scratch(m) },
			"SELECT scratch",
			StatusFail,
		},
		{"fetch", func(op, _ string) bool { return op == "FETCH" }, "BODY.PEEK", StatusFail},
		{"store", func(op, _ string) bool { return op == "STORE" }, "STORE keyword", StatusFail},
		{"idle", func(op, _ string) bool { return op == "IDLE" }, "IDLE push", StatusWarn},
		{"watcher select", secondScratchSelect(), "IDLE push", StatusFail},
		{"move", func(op, _ string) bool { return op == opMove }, "MOVE message", StatusFail},
		{"status", func(op, _ string) bool { return op == "STATUS" }, "MOVE message", StatusFail},
		{"delete", func(op, _ string) bool { return op == "DELETE" }, "DELETE scratch", StatusWarn},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			cfg := config(
				imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), Fail: tt.fail}),
			)
			cfg.Write = true
			cfg.IdleWait = 200 * time.Millisecond

			report, err := Run(cfg)
			require.NoError(t, err)

			expect(t, report, tt.check, tt.status, "")
		})
	}
}

func TestCheckSpecialFolder(t *testing.T) {
	t.Parallel()

	list := []*imap.ListData{
		{Mailbox: "INBOX"},
		{Mailbox: "Bulk", Attrs: []imap.MailboxAttr{imap.MailboxAttrJunk}},
		{Mailbox: "Sent Messages"},
	}

	report := &Report{}
	checkSpecialFolder(report, list, imap.MailboxAttrJunk, "Junk")
	checkSpecialFolder(report, list, imap.MailboxAttrSent, "Sent Messages")

	expect(t, report, `folder \Junk`, StatusOK, `"Bulk"`)
	expect(t, report, `folder \Sent`, StatusWarn, `guessed "Sent Messages"`)
}

// failWriter rejects every write.
type failWriter struct{}

func (failWriter) Write([]byte) (int, error) { return 0, errors.New("disk full") }

func TestWriteReportErrors(t *testing.T) {
	t.Parallel()

	report := &Report{Checks: []Check{{Name: "login", Status: StatusOK}}}

	tests := []struct {
		name  string
		write func(io.Writer) error
	}{
		{"text", report.WriteText},
		{"json", report.WriteJSON},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			require.Error(t, tt.write(failWriter{}), "a failing writer")
		})
	}
}

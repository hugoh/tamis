package probe

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/hugoh/tamis/internal/imaptest"
)

const opMove = "MOVE"

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
	if c.Status != status || !strings.Contains(c.Detail, detail) {
		t.Errorf("%s = %s %q, want %s containing %q", name, c.Status, c.Detail, status, detail)
	}
}

func TestRunReadOnly(t *testing.T) {
	t.Parallel()

	report, err := Run(t.Context(), config(imaptest.Start(t, imaptest.Rev2())))
	if err != nil {
		t.Fatal(err)
	}

	if report.Failed() {
		t.Errorf("unexpected failure: %+v", report.Checks)
	}

	expect(t, report, "login", StatusOK, imaptest.Username)
	expect(t, report, "IDLE", StatusOK, "advertised")
	expect(t, report, "CONDSTORE", StatusWarn, "missing")
	expect(t, report, "ENABLE QRESYNC", StatusSkip, "not advertised")
	expect(t, report, `folder \Junk`, StatusWarn, `guessed "Junk"`)
	expect(t, report, `folder \Sent`, StatusWarn, "not found")
	expect(t, report, "EXAMINE INBOX", StatusOK, "0 messages")
	expect(t, report, "HIGHESTMODSEQ", StatusSkip, "CONDSTORE")
	expect(t, report, "write checks", StatusSkip, "--write")

	if len(report.Mailboxes) != 2 {
		t.Errorf("mailboxes = %+v, want INBOX and Junk", report.Mailboxes)
	}
}

func TestRunWrite(t *testing.T) {
	t.Parallel()

	cfg := config(imaptest.Start(t, imaptest.Rev2()))
	cfg.Write = true

	report, err := Run(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}

	if report.Failed() {
		t.Errorf("unexpected failure: %+v", report.Checks)
	}

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

	report, err := Run(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}

	if !report.Failed() {
		t.Error("missing UIDPLUS should fail the report")
	}

	expect(t, report, "UIDPLUS", StatusFail, "missing")
	expect(t, report, opMove, StatusWarn, "missing")
	expect(t, report, "MOVE message", StatusOK, "unsafe outside scratch")
	expect(t, report, "DELETE scratch", StatusOK, "removed")
}

func TestRunTLS(t *testing.T) {
	t.Parallel()

	report, err := Run(t.Context(),
		config(imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true})),
	)
	if err != nil {
		t.Fatal(err)
	}

	expect(t, report, "login", StatusOK, "")
}

func TestRunBadPassword(t *testing.T) {
	t.Parallel()

	cfg := config(imaptest.Start(t, imaptest.Rev2()))
	cfg.Password = "wrong"

	if _, err := Run(t.Context(), cfg); !errors.Is(err, ErrLogin) {
		t.Errorf("err = %v, want ErrLogin", err)
	}
}

func TestRunConnectRefused(t *testing.T) {
	t.Parallel()

	ln, err := (&net.ListenConfig{}).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}

	addr := ln.Addr().String()
	_ = ln.Close()

	if _, err := Run(t.Context(), Config{Addr: addr, Plaintext: true}); err == nil {
		t.Error("want error dialing a closed port")
	}

	if _, err := rawEnable(t.Context(), Config{Addr: addr}); err == nil {
		t.Error("want error dialing a closed port over TLS")
	}
}

// fakeServer answers raw IMAP with a fixed reply to ENABLE.
func fakeServer(t *testing.T, enableReply string) Config {
	t.Helper()

	ln, err := (&net.ListenConfig{}).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = ln.Close() })

	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer func() { _ = conn.Close() }()

		_, _ = conn.Write([]byte("* OK ready\r\n"))
		r := bufio.NewReader(conn)

		for {
			line, err := r.ReadString('\n')
			if err != nil {
				return
			}

			tag, cmd, _ := strings.Cut(strings.TrimSpace(line), " ")

			switch {
			case strings.HasPrefix(cmd, "ENABLE"):
				_, _ = conn.Write([]byte(strings.ReplaceAll(enableReply, "TAG", tag)))
			default:
				_, _ = conn.Write([]byte(tag + " OK done\r\n"))
			}
		}
	}()

	return Config{Addr: ln.Addr().String(), Plaintext: true, Timeout: 5 * time.Second}
}

func TestCheckQResync(t *testing.T) {
	t.Parallel()

	caps := imap.CapSet{imap.CapQResync: {}, imap.CapEnable: {}}

	tests := []struct {
		name   string
		reply  string
		status Status
		detail string
	}{
		{"confirmed", "* ENABLED CONDSTORE QRESYNC\r\nTAG OK enabled\r\n", StatusOK, "QRESYNC"},
		{"silent", "TAG OK enabled\r\n", StatusWarn, "not confirmed"},
		{"rejected", "TAG BAD no\r\n", StatusFail, "rejected"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			report := &Report{}
			checkQResync(t.Context(), report, fakeServer(t, tt.reply), caps)
			expect(t, report, "ENABLE QRESYNC", tt.status, tt.detail)
		})
	}
}

func TestQuote(t *testing.T) {
	t.Parallel()

	if got, want := quote(`a"b\c`), `"a\"b\\c"`; got != want {
		t.Errorf("quote = %s, want %s", got, want)
	}
}

func TestWriteReport(t *testing.T) {
	t.Parallel()

	report := &Report{
		Server:       "imap.example.com:993",
		Capabilities: []string{"IDLE", opMove},
		Mailboxes:    []Mailbox{{Name: "Junk", Attrs: []string{`\Junk`}}},
		Checks:       []Check{{Name: opMove, Status: StatusOK, Detail: "native MOVE"}},
	}

	var text bytes.Buffer
	if err := report.WriteText(&text); err != nil {
		t.Fatal(err)
	}

	for _, want := range []string{"imap.example.com:993", "IDLE MOVE", "native MOVE", `\Junk`} {
		if !strings.Contains(text.String(), want) {
			t.Errorf("text report missing %q:\n%s", want, text.String())
		}
	}

	var buf bytes.Buffer
	if err := report.WriteJSON(&buf); err != nil {
		t.Fatal(err)
	}

	var decoded Report
	if err := json.Unmarshal(buf.Bytes(), &decoded); err != nil {
		t.Fatal(err)
	}

	if decoded.Checks[0].Status != StatusOK {
		t.Errorf("decoded = %+v", decoded)
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
			func(op, m string) bool { return op == "SELECT" && m == "INBOX" },
			"EXAMINE INBOX",
			StatusFail,
		},
		{"create", func(op, _ string) bool { return op == "CREATE" }, "CREATE scratch", StatusFail},
		{"append", func(op, _ string) bool { return op == "APPEND" }, "APPEND", StatusFail},
		{
			"select",
			func(op, m string) bool { return op == "SELECT" && scratch(m) },
			"SELECT scratch",
			StatusFail,
		},
		{"fetch", func(op, _ string) bool { return op == "FETCH" }, "BODY.PEEK", StatusFail},
		{"store", func(op, _ string) bool { return op == "STORE" }, "STORE keyword", StatusFail},
		{"idle", func(op, _ string) bool { return op == "IDLE" }, "IDLE push", StatusWarn},
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

			report, err := Run(t.Context(), cfg)
			if err != nil {
				t.Fatal(err)
			}

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

// Package probe checks which IMAP features a mail server actually supports,
// so tamis can pick a sync strategy before it touches real mail.
package probe

import (
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"slices"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"
)

// Status is the outcome of a single check.
type Status string

// Check outcomes.
const (
	StatusOK   Status = "ok"
	StatusWarn Status = "warn"
	StatusFail Status = "fail"
	StatusSkip Status = "skip"
)

const defaultTimeout = 30 * time.Second

// Config describes the account to probe.
type Config struct {
	// Addr is the server address, host:port.
	Addr     string
	Username string
	Password string
	// Write enables checks that create, fill and delete scratch folders.
	Write bool
	// IdleWait is how long to wait for an IDLE push before giving up.
	IdleWait time.Duration
	// Plaintext dials without TLS. Only for local test servers.
	Plaintext bool
	// TLSConfig overrides the default TLS configuration.
	TLSConfig *tls.Config
	// Timeout bounds dialing. Zero means 30 seconds.
	Timeout time.Duration
}

// Check is the result of probing one feature.
type Check struct {
	Name   string `json:"name"`
	Status Status `json:"status"`
	Detail string `json:"detail,omitempty"`
}

// Mailbox is a folder as reported by LIST.
type Mailbox struct {
	Name  string   `json:"name"`
	Attrs []string `json:"attrs,omitempty"`
}

// Report collects everything the probe learned.
type Report struct {
	Server       string    `json:"server"`
	Capabilities []string  `json:"capabilities"`
	Mailboxes    []Mailbox `json:"mailboxes"`
	Checks       []Check   `json:"checks"`
}

// Failed reports whether any check failed.
func (r *Report) Failed() bool {
	return slices.ContainsFunc(r.Checks, func(c Check) bool { return c.Status == StatusFail })
}

func (r *Report) addf(name string, status Status, format string, args ...any) {
	r.Checks = append(r.Checks, Check{
		Name:   name,
		Status: status,
		Detail: fmt.Sprintf(format, args...),
	})
}

// capability is an extension tamis cares about, and why.
type capability struct {
	cap      imap.Cap
	why      string
	required bool
}

func wantedCapabilities() []capability {
	return []capability{
		{imap.CapIdle, "push notification of new mail", true},
		{imap.CapUIDPlus, "safe UID EXPUNGE and APPENDUID", true},
		{imap.CapMove, "atomic moves (otherwise COPY + STORE + EXPUNGE)", false},
		{imap.CapSpecialUse, "automatic Junk/Sent folder discovery", false},
	}
}

// ErrLogin is returned when the server rejects the credentials.
var ErrLogin = errors.New("login failed")

// Run probes the server. It returns an error only when it cannot connect or
// log in; everything else is recorded as a check in the report.
func Run(cfg Config) (*Report, error) {
	report := &Report{Server: cfg.Addr}

	client, err := dial(cfg, nil)
	if err != nil {
		return nil, err
	}
	defer closeClient(client)

	report.addf("login", StatusOK, "authenticated as %s", cfg.Username)

	caps, err := client.Capability().Wait()
	if err != nil {
		return nil, fmt.Errorf("capability: %w", err)
	}

	report.Capabilities = sortedCaps(caps)
	checkCapabilities(report, caps)
	checkMailboxes(report, client, caps)
	checkInbox(report, client)

	if cfg.Write {
		runWriteChecks(report, cfg, client, caps)
	} else {
		report.addf("write checks", StatusSkip, "rerun with --write to test APPEND, IDLE, MOVE")
	}

	return report, nil
}

func dial(cfg Config, handler *imapclient.UnilateralDataHandler) (*imapclient.Client, error) {
	timeout := cfg.Timeout
	if timeout == 0 {
		timeout = defaultTimeout
	}

	options := &imapclient.Options{
		TLSConfig:             cfg.TLSConfig,
		Dialer:                &net.Dialer{Timeout: timeout},
		UnilateralDataHandler: handler,
	}

	var (
		client *imapclient.Client
		err    error
	)

	if cfg.Plaintext {
		client, err = imapclient.DialInsecure(cfg.Addr, options)
	} else {
		client, err = imapclient.DialTLS(cfg.Addr, options)
	}

	if err != nil {
		return nil, fmt.Errorf("connect to %s: %w", cfg.Addr, err)
	}

	if err := client.Login(cfg.Username, cfg.Password).Wait(); err != nil {
		closeClient(client)

		return nil, fmt.Errorf("%w: %w", ErrLogin, err)
	}

	return client, nil
}

func closeClient(client *imapclient.Client) {
	_ = client.Logout().Wait()
	_ = client.Close()
}

func sortedCaps(caps imap.CapSet) []string {
	names := make([]string, 0, len(caps))
	for c := range caps {
		names = append(names, string(c))
	}

	slices.Sort(names)

	return names
}

func checkCapabilities(report *Report, caps imap.CapSet) {
	for _, want := range wantedCapabilities() {
		switch {
		case caps.Has(want.cap):
			report.addf(string(want.cap), StatusOK, "advertised: %s", want.why)
		case want.required:
			report.addf(string(want.cap), StatusFail, "missing: %s", want.why)
		default:
			report.addf(string(want.cap), StatusWarn, "missing: %s", want.why)
		}
	}
}

func checkMailboxes(report *Report, client *imapclient.Client, caps imap.CapSet) {
	options := &imap.ListOptions{ReturnSpecialUse: caps.Has(imap.CapSpecialUse)}

	list, err := client.List("", "*", options).Collect()
	if err != nil {
		report.addf("LIST", StatusFail, "%v", err)

		return
	}

	for _, mbox := range list {
		attrs := make([]string, 0, len(mbox.Attrs))
		for _, a := range mbox.Attrs {
			attrs = append(attrs, string(a))
		}

		report.Mailboxes = append(report.Mailboxes, Mailbox{Name: mbox.Mailbox, Attrs: attrs})
	}

	report.addf("LIST", StatusOK, "%d folders", len(list))
	checkSpecialFolder(report, list, imap.MailboxAttrJunk, "Junk")
	checkSpecialFolder(report, list, imap.MailboxAttrSent, "Sent Messages", "Sent")
}

// checkSpecialFolder looks for a folder by SPECIAL-USE attribute, falling back
// to well-known names.
func checkSpecialFolder(
	report *Report,
	list []*imap.ListData,
	attr imap.MailboxAttr,
	names ...string,
) {
	name := "folder " + string(attr)

	for _, mbox := range list {
		if slices.Contains(mbox.Attrs, attr) {
			report.addf(name, StatusOK, "%q", mbox.Mailbox)

			return
		}
	}

	for _, mbox := range list {
		if slices.Contains(names, mbox.Mailbox) {
			report.addf(name, StatusWarn, "no attribute; guessed %q by name", mbox.Mailbox)

			return
		}
	}

	report.addf(name, StatusWarn, "not found")
}

func checkInbox(report *Report, client *imapclient.Client) {
	options := &imap.SelectOptions{ReadOnly: true}

	data, err := client.Select("INBOX", options).Wait()
	if err != nil {
		report.addf("EXAMINE INBOX", StatusFail, "%v", err)

		return
	}

	report.addf("EXAMINE INBOX", StatusOK, "%d messages, UIDVALIDITY %d, UIDNEXT %d",
		data.NumMessages, data.UIDValidity, data.UIDNext)

	if slices.Contains(data.PermanentFlags, imap.FlagWildcard) {
		report.addf("custom keywords", StatusOK, `PERMANENTFLAGS includes \*`)
	} else {
		report.addf(
			"custom keywords",
			StatusWarn,
			"server may not keep keywords such as $TamisFiled",
		)
	}
}

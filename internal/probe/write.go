package probe

import (
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"
)

var errNotFound = errors.New("message not found")

const (
	defaultIdleWait = 30 * time.Second
	probeKeyword    = imap.Flag("$TamisProbe")
)

// scratch names the two throwaway folders the write checks use. They are
// top-level and timestamped so they never collide with real mail.
type scratch struct {
	src, dst string
}

func newScratch(now time.Time) scratch {
	base := fmt.Sprintf("tamis-probe-%d", now.Unix())

	return scratch{src: base, dst: base + "-moved"}
}

func runWriteChecks(report *Report, cfg Config, client *imapclient.Client, caps imap.CapSet) {
	folders := newScratch(time.Now())

	if !createScratch(report, client, folders) {
		return
	}
	defer deleteScratch(report, client, folders)

	uid, ok := appendMessage(report, client, folders.src)
	if !ok {
		return
	}

	if _, err := client.Select(folders.src, nil).Wait(); err != nil {
		report.addf("SELECT scratch", StatusFail, "%v", err)

		return
	}

	checkPeek(report, client, uid)
	checkKeyword(report, client, uid)
	checkIdle(report, cfg, client, caps, folders.src)
	checkMove(report, client, caps, uid, folders.dst)
}

func createScratch(report *Report, client *imapclient.Client, folders scratch) bool {
	for _, name := range []string{folders.src, folders.dst} {
		if err := client.Create(name, nil).Wait(); err != nil {
			report.addf("CREATE scratch", StatusFail, "%s: %v", name, err)

			return false
		}
	}

	report.addf("CREATE scratch", StatusOK, "%s, %s", folders.src, folders.dst)

	return true
}

func deleteScratch(report *Report, client *imapclient.Client, folders scratch) {
	// A folder can't be deleted while selected on some servers.
	_, _ = client.Select("INBOX", &imap.SelectOptions{ReadOnly: true}).Wait()

	var failed []string

	for _, name := range []string{folders.src, folders.dst} {
		if err := client.Delete(name).Wait(); err != nil {
			failed = append(failed, fmt.Sprintf("%s: %v", name, err))
		}
	}

	if len(failed) > 0 {
		report.addf("DELETE scratch", StatusWarn, "delete by hand: %v", failed)
	} else {
		report.addf("DELETE scratch", StatusOK, "scratch folders removed")
	}
}

func testMessage(now time.Time) []byte {
	return fmt.Appendf(nil, "From: tamis probe <probe@tamis.invalid>\r\n"+
		"To: probe@tamis.invalid\r\n"+
		"Subject: tamis probe\r\n"+
		"Date: %s\r\n"+
		"Message-ID: <%d.probe@tamis.invalid>\r\n"+
		"\r\n"+
		"Test message written by tamis probe. Safe to delete.\r\n",
		now.Format(time.RFC1123Z), now.UnixNano())
}

// appendMessage appends a test message and returns its UID.
func appendMessage(report *Report, client *imapclient.Client, mailbox string) (imap.UID, bool) {
	data, err := appendTo(client, mailbox)
	if err != nil {
		report.addf("APPEND", StatusFail, "%v", err)

		return 0, false
	}

	if data.UID == 0 {
		report.addf("APPEND", StatusFail, "no APPENDUID: remaining write checks skipped")

		return 0, false
	}

	report.addf("APPEND", StatusOK, "APPENDUID returned UID %d", data.UID)

	return data.UID, true
}

func appendTo(client *imapclient.Client, mailbox string) (*imap.AppendData, error) {
	msg := testMessage(time.Now())

	cmd := client.Append(mailbox, int64(len(msg)), nil)
	if _, err := cmd.Write(msg); err != nil {
		return nil, fmt.Errorf("append: %w", err)
	}

	if err := cmd.Close(); err != nil {
		return nil, fmt.Errorf("append: %w", err)
	}

	data, err := cmd.Wait()
	if err != nil {
		return nil, fmt.Errorf("append: %w", err)
	}

	return data, nil
}

func fetchFlags(client *imapclient.Client, uid imap.UID) ([]imap.Flag, error) {
	msgs, err := client.Fetch(imap.UIDSetNum(uid), &imap.FetchOptions{Flags: true}).Collect()
	if err != nil {
		return nil, fmt.Errorf("fetch flags: %w", err)
	}

	if len(msgs) == 0 {
		return nil, fmt.Errorf("fetch flags: UID %d: %w", uid, errNotFound)
	}

	return msgs[0].Flags, nil
}

// hasFlag reports whether flags contains flag. Flags and keywords are
// case-insensitive (RFC 9051 §2.3.2).
func hasFlag(flags []imap.Flag, flag imap.Flag) bool {
	return slices.ContainsFunc(flags, func(f imap.Flag) bool {
		return strings.EqualFold(string(f), string(flag))
	})
}

// flagCheck runs an action on the test message, then checks whether a flag
// ended up set.
type flagCheck struct {
	name    string
	flag    imap.Flag
	wantSet bool
	ok, bad string
}

func (fc flagCheck) run(
	report *Report,
	client *imapclient.Client,
	uid imap.UID,
	action func() error,
) {
	if err := action(); err != nil {
		report.addf(fc.name, StatusFail, "%v", err)

		return
	}

	flags, err := fetchFlags(client, uid)

	switch {
	case err != nil:
		report.addf(fc.name, StatusFail, "%v", err)
	case hasFlag(flags, fc.flag) == fc.wantSet:
		report.addf(fc.name, StatusOK, "%s", fc.ok)
	default:
		report.addf(fc.name, StatusFail, "%s", fc.bad)
	}
}

// checkPeek confirms BODY.PEEK[] leaves the message unread, so scanning mail
// never marks it as seen.
func checkPeek(report *Report, client *imapclient.Client, uid imap.UID) {
	check := flagCheck{
		name: "BODY.PEEK",
		flag: imap.FlagSeen,
		ok:   "message still unread after BODY.PEEK[]",
		bad:  `peek set \Seen: scanning would mark mail as read`,
	}

	check.run(report, client, uid, func() error {
		options := &imap.FetchOptions{BodySection: []*imap.FetchItemBodySection{{Peek: true}}}
		_, err := client.Fetch(imap.UIDSetNum(uid), options).Collect()

		return err //nolint:wrapcheck // reported verbatim
	})
}

// checkKeyword confirms the server stores custom keywords, which tamis uses
// to recognise messages it has already filed.
func checkKeyword(report *Report, client *imapclient.Client, uid imap.UID) {
	check := flagCheck{
		name:    "STORE keyword",
		flag:    probeKeyword,
		wantSet: true,
		ok:      string(probeKeyword) + " kept",
		bad:     string(probeKeyword) + " silently dropped",
	}

	check.run(report, client, uid, func() error {
		store := &imap.StoreFlags{
			Op:     imap.StoreFlagsAdd,
			Silent: true,
			Flags:  []imap.Flag{probeKeyword},
		}

		return client.Store(imap.UIDSetNum(uid), store, nil).Close()
	})
}

// checkIdle opens a second connection that IDLEs on the scratch folder, then
// appends from the first and times how long the push takes. It also shows
// the server accepts two concurrent connections.
func checkIdle(
	report *Report,
	cfg Config,
	client *imapclient.Client,
	caps imap.CapSet,
	mailbox string,
) {
	const name = "IDLE push"

	if !caps.Has(imap.CapIdle) {
		report.addf(name, StatusSkip, "IDLE not advertised")

		return
	}

	pushed := make(chan struct{}, 1)
	handler := &imapclient.UnilateralDataHandler{
		Mailbox: func(data *imapclient.UnilateralDataMailbox) {
			if data.NumMessages != nil {
				select {
				case pushed <- struct{}{}:
				default:
				}
			}
		},
	}

	watcher, err := dial(cfg, handler)
	if err != nil {
		report.addf("second connection", StatusFail, "%v", err)

		return
	}
	defer closeClient(watcher)

	report.addf("second connection", StatusOK, "two concurrent logins accepted")

	if _, err := watcher.Select(mailbox, nil).Wait(); err != nil {
		report.addf(name, StatusFail, "%v", err)

		return
	}

	idle, err := watcher.Idle()
	if err != nil {
		report.addf(name, StatusFail, "%v", err)

		return
	}

	wait := cfg.IdleWait
	if wait == 0 {
		wait = defaultIdleWait
	}

	status, detail := waitForPush(client, mailbox, wait, pushed)
	report.addf(name, status, "%s", detail)

	if err := idle.Close(); err != nil {
		report.addf(name, StatusWarn, "stopping IDLE: %v", err)
	}
}

func waitForPush(
	client *imapclient.Client,
	mailbox string,
	wait time.Duration,
	pushed <-chan struct{},
) (Status, string) {
	start := time.Now()

	if _, err := appendTo(client, mailbox); err != nil {
		return StatusFail, fmt.Sprintf("append failed, push not tested: %v", err)
	}

	select {
	case <-pushed:
		return StatusOK, fmt.Sprintf(
			"new mail pushed after %s",
			time.Since(start).Round(time.Millisecond),
		)
	case <-time.After(wait):
		return StatusWarn, fmt.Sprintf("no push within %s: poll this folder instead", wait)
	}
}

func checkMove(
	report *Report,
	client *imapclient.Client,
	caps imap.CapSet,
	uid imap.UID,
	dst string,
) {
	const name = "MOVE message"

	method := "native MOVE"

	switch {
	case caps.Has(imap.CapMove):
	case caps.Has(imap.CapUIDPlus):
		method = "fallback COPY + STORE + UID EXPUNGE"
	default:
		method = "fallback COPY + STORE + EXPUNGE (unsafe outside scratch folders)"
	}

	if _, err := client.Move(imap.UIDSetNum(uid), dst).Wait(); err != nil {
		report.addf(name, StatusFail, "%s: %v", method, err)

		return
	}

	status, err := client.Status(dst, &imap.StatusOptions{NumMessages: true}).Wait()

	switch {
	case err != nil:
		report.addf(name, StatusFail, "%s: %v", method, err)
	case status.NumMessages == nil || *status.NumMessages != 1:
		report.addf(name, StatusFail, "%s: message not found in %s", method, dst)
	default:
		report.addf(name, StatusOK, "%s", method)
	}
}

package imaptest

import (
	"errors"
	"fmt"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
)

// idlePollInterval is how often Idle checks for updates.
const idlePollInterval = 10 * time.Millisecond

var (
	// errInjected is what an injected failure returns to the client.
	errInjected = &imap.Error{Type: imap.StatusResponseTypeNo, Text: "injected failure"}

	errUnsupported = errors.New("imaptest: session lacks NAMESPACE or MOVE support")
)

// fullSession is what the in-memory server's sessions implement.
type fullSession interface {
	imapserver.Session
	imapserver.SessionNamespace
	imapserver.SessionMove
}

// session wraps the in-memory session and fails the commands Options.Fail
// selects. Passthrough errors are returned unwrapped so clients see the
// server's own responses.
type session struct {
	fullSession

	fail func(op, mailbox string) bool
}

func wrap(inner imapserver.Session, fail func(op, mailbox string) bool) (*session, error) {
	full, ok := inner.(fullSession)
	if !ok {
		return nil, fmt.Errorf("%w: %T", errUnsupported, inner)
	}

	if fail == nil {
		fail = func(string, string) bool { return false }
	}

	return &session{fullSession: full, fail: fail}, nil
}

func (s *session) Select(mailbox string, options *imap.SelectOptions) (*imap.SelectData, error) {
	if err := s.check("SELECT", mailbox); err != nil {
		return nil, err
	}

	return s.fullSession.Select(mailbox, options) //nolint:wrapcheck // passthrough
}

func (s *session) Create(mailbox string, options *imap.CreateOptions) error {
	if err := s.check("CREATE", mailbox); err != nil {
		return err
	}

	return s.fullSession.Create(mailbox, options) //nolint:wrapcheck // passthrough
}

func (s *session) Delete(mailbox string) error {
	if err := s.check("DELETE", mailbox); err != nil {
		return err
	}

	return s.fullSession.Delete(mailbox) //nolint:wrapcheck // passthrough
}

func (s *session) List(
	writer *imapserver.ListWriter,
	ref string,
	patterns []string,
	options *imap.ListOptions,
) error {
	if err := s.check("LIST", ""); err != nil {
		return err
	}

	return s.fullSession.List(writer, ref, patterns, options) //nolint:wrapcheck // passthrough
}

func (s *session) Status(mailbox string, options *imap.StatusOptions) (*imap.StatusData, error) {
	if err := s.check("STATUS", mailbox); err != nil {
		return nil, err
	}

	return s.fullSession.Status(mailbox, options) //nolint:wrapcheck // passthrough
}

func (s *session) Append(
	mailbox string,
	reader imap.LiteralReader,
	options *imap.AppendOptions,
) (*imap.AppendData, error) {
	if err := s.check("APPEND", mailbox); err != nil {
		return nil, err
	}

	return s.fullSession.Append(mailbox, reader, options) //nolint:wrapcheck // passthrough
}

func (s *session) Fetch(
	writer *imapserver.FetchWriter,
	numSet imap.NumSet,
	options *imap.FetchOptions,
) error {
	if err := s.check("FETCH", ""); err != nil {
		return err
	}

	return s.fullSession.Fetch(writer, numSet, options) //nolint:wrapcheck // passthrough
}

func (s *session) Store(
	writer *imapserver.FetchWriter,
	numSet imap.NumSet,
	flags *imap.StoreFlags,
	options *imap.StoreOptions,
) error {
	if err := s.check("STORE", ""); err != nil {
		return err
	}

	return s.fullSession.Store(writer, numSet, flags, options) //nolint:wrapcheck // passthrough
}

// Idle serves IDLE by polling the session's update queue instead of
// delegating to the in-memory server. Its tracker only wakes Idle for
// updates queued after Idle registers, which happens after the server has
// already sent "+ idling", so an APPEND from another connection in that gap
// was never pushed and IDLE tests flaked under load.
func (s *session) Idle(writer *imapserver.UpdateWriter, stop <-chan struct{}) error {
	if err := s.check("IDLE", ""); err != nil {
		return err
	}

	ticker := time.NewTicker(idlePollInterval)
	defer ticker.Stop()

	for {
		if err := s.Poll(writer, true); err != nil {
			return err //nolint:wrapcheck // passthrough
		}

		select {
		case <-stop:
			return nil
		case <-ticker.C:
		}
	}
}

func (s *session) Move(writer *imapserver.MoveWriter, numSet imap.NumSet, dest string) error {
	if err := s.check("MOVE", dest); err != nil {
		return err
	}

	return s.fullSession.Move(writer, numSet, dest) //nolint:wrapcheck // passthrough
}

func (s *session) check(op, mailbox string) error {
	if s.fail(op, mailbox) {
		return errInjected
	}

	return nil
}

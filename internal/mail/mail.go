// Package mail is tamis's IMAP client: the handful of mailbox operations the
// engine needs, with the safety rules from the design built in (fetches
// never mark mail as read; moves never fall back to a bare EXPUNGE).
package mail

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"slices"
	"strings"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"
)

// MaxMessageBytes caps how much of each message is downloaded; enough for
// headers and the text that classification looks at.
const MaxMessageBytes = 256 << 10

const dialTimeout = 30 * time.Second

var (
	// ErrLogin means the server rejected the credentials.
	ErrLogin = errors.New("login failed")
	// ErrUnsafeMove means the server has neither MOVE nor UIDPLUS, so the
	// only way to move would be an EXPUNGE that could purge other messages
	// already marked \Deleted.
	ErrUnsafeMove = errors.New("server supports neither MOVE nor UIDPLUS; refusing an unsafe move")
)

// DialConfig says how to connect.
type DialConfig struct {
	Addr     string
	Username string
	Password string
	// Plaintext dials without TLS. Only for local test servers.
	Plaintext bool
	TLSConfig *tls.Config
}

// Client is a logged-in IMAP connection. It is not safe for concurrent use.
type Client struct {
	imap     *imapclient.Client
	selected string
}

// Dial connects and logs in.
func Dial(_ context.Context, cfg DialConfig) (*Client, error) {
	options := &imapclient.Options{
		TLSConfig: cfg.TLSConfig,
		Dialer:    &net.Dialer{Timeout: dialTimeout},
	}

	var (
		conn *imapclient.Client
		err  error
	)

	if cfg.Plaintext {
		conn, err = imapclient.DialInsecure(cfg.Addr, options)
	} else {
		conn, err = imapclient.DialTLS(cfg.Addr, options)
	}

	if err != nil {
		return nil, fmt.Errorf("connect to %s: %w", cfg.Addr, err)
	}

	if err := conn.Login(cfg.Username, cfg.Password).Wait(); err != nil {
		_ = conn.Close()

		return nil, fmt.Errorf("%w: %w", ErrLogin, err)
	}

	return &Client{imap: conn}, nil
}

// Close logs out and closes the connection.
func (c *Client) Close() error {
	_ = c.imap.Logout().Wait()

	if err := c.imap.Close(); err != nil {
		return fmt.Errorf("close connection: %w", err)
	}

	return nil
}

// State is a folder's UIDVALIDITY and next UID.
type State struct {
	UIDValidity uint32
	UIDNext     uint32
}

// State returns the folder's current UIDVALIDITY and UIDNEXT.
func (c *Client) State(mailbox string) (State, error) {
	status, err := c.imap.Status(mailbox, &imap.StatusOptions{UIDNext: true, UIDValidity: true}).
		Wait()
	if err != nil {
		return State{}, fmt.Errorf("status %s: %w", mailbox, err)
	}

	return State{UIDValidity: status.UIDValidity, UIDNext: uint32(status.UIDNext)}, nil
}

// UIDsAfter lists the UIDs in mailbox greater than after, ascending.
func (c *Client) UIDsAfter(mailbox string, after uint32) ([]uint32, error) {
	var newer imap.UIDSet
	newer.AddRange(imap.UID(after+1), 0)

	uids, err := c.search(mailbox, &imap.SearchCriteria{UID: []imap.UIDSet{newer}})
	if err != nil {
		return nil, err
	}

	// "n:*" matches the highest UID even when it is below n.
	return slices.DeleteFunc(uids, func(uid uint32) bool { return uid <= after }), nil
}

// SeenBefore lists read messages that arrived before the given day.
func (c *Client) SeenBefore(mailbox string, before time.Time) ([]uint32, error) {
	return c.search(mailbox, &imap.SearchCriteria{Flag: []imap.Flag{imap.FlagSeen}, Before: before})
}

// Message is a fetched message: the first MaxMessageBytes of its source and
// the envelope fields tamis records.
type Message struct {
	UID       uint32
	MessageID string
	From      string
	Subject   string
	// Flags are lower-cased: IMAP flags and keywords are case-insensitive.
	Flags []string
	Raw   []byte
}

// Fetch downloads the given messages without marking them as read.
func (c *Client) Fetch(mailbox string, uids []uint32) ([]Message, error) {
	options := &imap.FetchOptions{
		UID:      true,
		Flags:    true,
		Envelope: true,
		BodySection: []*imap.FetchItemBodySection{{
			Peek:    true,
			Partial: &imap.SectionPartial{Offset: 0, Size: MaxMessageBytes},
		}},
	}

	buffers, err := c.fetch(mailbox, uids, options)
	if err != nil {
		return nil, err
	}

	messages := make([]Message, 0, len(buffers))

	for _, buffer := range buffers {
		message := Message{UID: uint32(buffer.UID)}

		for _, flag := range buffer.Flags {
			message.Flags = append(message.Flags, strings.ToLower(string(flag)))
		}

		if len(buffer.BodySection) > 0 {
			message.Raw = buffer.BodySection[0].Bytes
		}

		if envelope := buffer.Envelope; envelope != nil {
			message.Subject = envelope.Subject
			message.MessageID = bracket(envelope.MessageID)

			if len(envelope.From) > 0 {
				message.From = strings.ToLower(envelope.From[0].Addr())
			}
		}

		messages = append(messages, message)
	}

	return messages, nil
}

// Recipients returns the distinct To and Cc addresses of the given
// messages, lower-cased.
func (c *Client) Recipients(mailbox string, uids []uint32) ([]string, error) {
	buffers, err := c.fetch(mailbox, uids, &imap.FetchOptions{Envelope: true})
	if err != nil {
		return nil, err
	}

	seen := map[string]bool{}

	var addresses []string

	for _, buffer := range buffers {
		if buffer.Envelope == nil {
			continue
		}

		for _, addr := range slices.Concat(buffer.Envelope.To, buffer.Envelope.Cc) {
			address := strings.ToLower(addr.Addr())
			if address != "" && !seen[address] {
				seen[address] = true
				addresses = append(addresses, address)
			}
		}
	}

	return addresses, nil
}

// Move moves messages to dest, natively with MOVE or with COPY, STORE
// \Deleted and UID EXPUNGE. It refuses servers that offer neither MOVE nor
// UIDPLUS.
func (c *Client) Move(mailbox string, uids []uint32, dest string) error {
	caps := c.imap.Caps()
	if !caps.Has(imap.CapMove) && !caps.Has(imap.CapUIDPlus) {
		return ErrUnsafeMove
	}

	if err := c.selectMailbox(mailbox); err != nil {
		return err
	}

	if _, err := c.imap.Move(uidSet(uids), dest).Wait(); err != nil {
		return fmt.Errorf("move from %s to %s: %w", mailbox, dest, err)
	}

	return nil
}

// AddKeyword sets a flag or keyword on the given messages.
func (c *Client) AddKeyword(mailbox string, uids []uint32, keyword string) error {
	return c.storeKeyword(mailbox, uids, keyword, imap.StoreFlagsAdd)
}

// RemoveKeyword clears a flag or keyword from the given messages.
func (c *Client) RemoveKeyword(mailbox string, uids []uint32, keyword string) error {
	return c.storeKeyword(mailbox, uids, keyword, imap.StoreFlagsDel)
}

// Append adds a message to mailbox and returns its UID (0 if the server
// doesn't report one).
func (c *Client) Append(
	mailbox string,
	raw []byte,
	flags []string,
	date time.Time,
) (uint32, error) {
	options := &imap.AppendOptions{Time: date}
	for _, flag := range flags {
		options.Flags = append(options.Flags, imap.Flag(flag))
	}

	cmd := c.imap.Append(mailbox, int64(len(raw)), options)
	if _, err := cmd.Write(raw); err != nil {
		return 0, fmt.Errorf("append to %s: %w", mailbox, err)
	}

	if err := cmd.Close(); err != nil {
		return 0, fmt.Errorf("append to %s: %w", mailbox, err)
	}

	data, err := cmd.Wait()
	if err != nil {
		return 0, fmt.Errorf("append to %s: %w", mailbox, err)
	}

	return uint32(data.UID), nil
}

// Ensure creates mailbox unless it already exists.
func (c *Client) Ensure(mailbox string) error {
	existing, err := c.imap.List("", mailbox, nil).Collect()
	if err != nil {
		return fmt.Errorf("list %s: %w", mailbox, err)
	}

	if len(existing) > 0 {
		return nil
	}

	if err := c.imap.Create(mailbox, nil).Wait(); err != nil {
		return fmt.Errorf("create %s: %w", mailbox, err)
	}

	return nil
}

func (c *Client) search(mailbox string, criteria *imap.SearchCriteria) ([]uint32, error) {
	if err := c.selectMailbox(mailbox); err != nil {
		return nil, err
	}

	data, err := c.imap.UIDSearch(criteria, nil).Wait()
	if err != nil {
		return nil, fmt.Errorf("search %s: %w", mailbox, err)
	}

	all := data.AllUIDs()
	uids := make([]uint32, len(all))

	for i, uid := range all {
		uids[i] = uint32(uid)
	}

	slices.Sort(uids)

	return uids, nil
}

func (c *Client) fetch(
	mailbox string,
	uids []uint32,
	options *imap.FetchOptions,
) ([]*imapclient.FetchMessageBuffer, error) {
	// An empty UID set can't be encoded; sending one breaks the connection.
	if len(uids) == 0 {
		return nil, nil
	}

	if err := c.selectMailbox(mailbox); err != nil {
		return nil, err
	}

	buffers, err := c.imap.Fetch(uidSet(uids), options).Collect()
	if err != nil {
		return nil, fmt.Errorf("fetch from %s: %w", mailbox, err)
	}

	return buffers, nil
}

func (c *Client) storeKeyword(
	mailbox string,
	uids []uint32,
	keyword string,
	operation imap.StoreFlagsOp,
) error {
	if err := c.selectMailbox(mailbox); err != nil {
		return err
	}

	store := &imap.StoreFlags{Op: operation, Silent: true, Flags: []imap.Flag{imap.Flag(keyword)}}
	if err := c.imap.Store(uidSet(uids), store, nil).Close(); err != nil {
		return fmt.Errorf("update %s in %s: %w", keyword, mailbox, err)
	}

	return nil
}

func (c *Client) selectMailbox(mailbox string) error {
	if c.selected == mailbox {
		return nil
	}

	if _, err := c.imap.Select(mailbox, nil).Wait(); err != nil {
		c.selected = ""

		return fmt.Errorf("select %s: %w", mailbox, err)
	}

	c.selected = mailbox

	return nil
}

func uidSet(uids []uint32) imap.UIDSet {
	set := make([]imap.UID, len(uids))
	for i, uid := range uids {
		set[i] = imap.UID(uid)
	}

	return imap.UIDSetNum(set...)
}

// bracket normalises a Message-ID to its <...> form.
func bracket(messageID string) string {
	if messageID == "" || strings.HasPrefix(messageID, "<") {
		return messageID
	}

	return "<" + messageID + ">"
}

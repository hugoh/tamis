// Package mail is tamis's IMAP client: the handful of mailbox operations the
// engine needs, with the safety rules from the design built in (fetches
// never mark mail as read; moves never fall back to a bare EXPUNGE).
package mail

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"
)

// MaxMessageBytes caps how much of each message is downloaded; enough for
// headers and the text that classification looks at.
const MaxMessageBytes = 256 << 10

const (
	dialTimeout = 30 * time.Second
	// idleStopTimeout bounds how long ending IDLE may take before the
	// connection is dropped.
	idleStopTimeout = 5 * time.Second
)

var (
	// ErrLogin means the server rejected the credentials.
	ErrLogin = errors.New("login failed")
	// ErrUnsafeMove means the server has neither MOVE nor UIDPLUS, so the
	// only way to move would be an EXPUNGE that could purge other messages
	// already marked \Deleted.
	ErrUnsafeMove = errors.New("server supports neither MOVE nor UIDPLUS; refusing an unsafe move")
	// ErrNoIdle means the server doesn't advertise IDLE.
	ErrNoIdle = errors.New("server does not support IDLE")
)

// DialConfig says how to connect.
type DialConfig struct {
	Addr     string
	Username string
	Password string
	// Plaintext dials without TLS. Only for local test servers.
	Plaintext bool
	TLSConfig *tls.Config
	// Logger receives a debug line per IMAP exchange; nil discards them.
	// Credentials and message contents are never logged.
	Logger *slog.Logger
}

// Client is a logged-in IMAP connection. It is not safe for concurrent use.
type Client struct {
	imap     *imapclient.Client
	log      *slog.Logger
	addr     string
	selected string
}

// Dial connects and logs in.
func Dial(ctx context.Context, cfg DialConfig) (*Client, error) {
	return dial(ctx, cfg, nil)
}

func dial(
	ctx context.Context,
	cfg DialConfig,
	handler *imapclient.UnilateralDataHandler,
) (*Client, error) {
	logger := cfg.Logger
	if logger == nil {
		logger = slog.New(slog.DiscardHandler)
	}

	options := &imapclient.Options{
		TLSConfig:             cfg.TLSConfig,
		Dialer:                &net.Dialer{Timeout: dialTimeout},
		UnilateralDataHandler: handler,
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

	logger.DebugContext(ctx, "connected", "addr", cfg.Addr, "plaintext", cfg.Plaintext)

	return &Client{imap: conn, log: logger, addr: cfg.Addr}, nil
}

// Close logs out and closes the connection.
func (c *Client) Close() error {
	_ = c.imap.Logout().Wait()

	err := c.imap.Close()

	c.log.Debug("disconnected", "addr", c.addr)

	if err != nil {
		return fmt.Errorf("close connection: %w", err)
	}

	return nil
}

// CloseWhenDone drops the connection when ctx ends, which unblocks a command
// waiting on a server that stopped answering. The returned function cancels
// it.
func (c *Client) CloseWhenDone(ctx context.Context) func() bool {
	return context.AfterFunc(ctx, func() {
		_ = c.imap.Close()

		c.log.Debug("connection dropped: its context ended", "addr", c.addr)
	})
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

	c.log.Debug("status", "mailbox", mailbox, "uidvalidity", status.UIDValidity,
		"uidnext", status.UIDNext)

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

// FindMessageID lists the UIDs in mailbox whose Message-ID header contains
// messageID. An empty ID matches nothing.
func (c *Client) FindMessageID(mailbox, messageID string) ([]uint32, error) {
	if messageID == "" {
		return nil, nil
	}

	return c.search(mailbox, &imap.SearchCriteria{
		Header: []imap.SearchCriteriaHeaderField{{Key: "Message-ID", Value: messageID}},
	})
}

// SearchKeywords lists messages carrying any of the keywords that arrived
// on or after since (zero for no bound). No keywords match nothing.
func (c *Client) SearchKeywords(
	mailbox string,
	keywords []string,
	since time.Time,
) ([]uint32, error) {
	if len(keywords) == 0 {
		return nil, nil
	}

	criteria := imap.SearchCriteria{Flag: []imap.Flag{imap.Flag(keywords[0])}}

	for _, keyword := range keywords[1:] {
		criteria = imap.SearchCriteria{Or: [][2]imap.SearchCriteria{
			{criteria, {Flag: []imap.Flag{imap.Flag(keyword)}}},
		}}
	}

	criteria.Since = since

	return c.search(mailbox, &criteria)
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
	return c.fetchMessages(mailbox, uids, true)
}

// FetchEnvelopes is Fetch without the message source: UID, flags and
// envelope fields only.
func (c *Client) FetchEnvelopes(mailbox string, uids []uint32) ([]Message, error) {
	return c.fetchMessages(mailbox, uids, false)
}

func toMessage(buffer *imapclient.FetchMessageBuffer) Message {
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

	return message
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

	c.log.Debug("move", "mailbox", mailbox, "dest", dest, "messages", len(uids))

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

	c.log.Debug("append", "mailbox", mailbox, "bytes", len(raw), "uid", data.UID)

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

	c.log.Debug("create", "mailbox", mailbox)

	return nil
}

func (c *Client) fetchMessages(mailbox string, uids []uint32, withBody bool) ([]Message, error) {
	options := &imap.FetchOptions{UID: true, Flags: true, Envelope: true}
	if withBody {
		options.BodySection = []*imap.FetchItemBodySection{{
			Peek:    true,
			Partial: &imap.SectionPartial{Offset: 0, Size: MaxMessageBytes},
		}}
	}

	buffers, err := c.fetch(mailbox, uids, options)
	if err != nil {
		return nil, err
	}

	messages := make([]Message, 0, len(buffers))

	for _, buffer := range buffers {
		messages = append(messages, toMessage(buffer))
	}

	return messages, nil
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

	c.log.Debug("search", "mailbox", mailbox, "matches", len(uids))

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

	c.log.Debug("fetch", "mailbox", mailbox, "requested", len(uids), "returned", len(buffers))

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

	c.log.Debug("store", "mailbox", mailbox, "keyword", keyword, "op", operation,
		"messages", len(uids))

	return nil
}

func (c *Client) selectMailbox(mailbox string) error {
	if c.selected == mailbox {
		return nil
	}

	_, err := c.selectData(mailbox)

	return err
}

func (c *Client) selectData(mailbox string) (*imap.SelectData, error) {
	data, err := c.imap.Select(mailbox, nil).Wait()
	if err != nil {
		c.selected = ""

		return nil, fmt.Errorf("select %s: %w", mailbox, err)
	}

	c.selected = mailbox

	c.log.Debug("select", "mailbox", mailbox)

	return data, nil
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

// WaitForNewMail returns when mailbox receives a message, after maxWait, or
// when ctx ends, whichever comes first. seen is the folder state the caller
// last handled: if the folder already holds newer mail, or has a new
// UIDVALIDITY, it returns at once. It IDLEs on its own connection, so it can
// run between passes without disturbing them. It returns ErrNoIdle without
// waiting when the server doesn't advertise IDLE.
func WaitForNewMail(
	ctx context.Context,
	cfg DialConfig,
	mailbox string,
	seen State,
	maxWait time.Duration,
) error {
	arrived := make(chan struct{}, 1)
	handler := &imapclient.UnilateralDataHandler{
		Mailbox: func(data *imapclient.UnilateralDataMailbox) {
			if data.NumMessages != nil {
				select {
				case arrived <- struct{}{}:
				default:
				}
			}
		},
	}

	client, err := dial(ctx, cfg, handler)
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	if !client.imap.Caps().Has(imap.CapIdle) {
		return ErrNoIdle
	}

	data, err := client.selectData(mailbox)
	if err != nil {
		return err
	}

	if data.UIDValidity != seen.UIDValidity || uint32(data.UIDNext) > seen.UIDNext {
		client.log.DebugContext(
			ctx,
			"idle ended",
			"mailbox",
			mailbox,
			"reason",
			"mail already waiting",
		)

		return nil
	}

	return client.idleUntil(ctx, mailbox, arrived, maxWait)
}

// Watch is a folder and the state the caller last handled in it.
type Watch struct {
	Mailbox string
	Seen    State
}

// WaitForNewMailIn is WaitForNewMail over several folders: it IDLEs on one
// connection per folder and returns as soon as any of them has mail, ends
// or fails. That result is the one returned; the other waits are stopped.
func WaitForNewMailIn(
	ctx context.Context,
	cfg DialConfig,
	watches []Watch,
	maxWait time.Duration,
) error {
	ctx, cancel := context.WithCancel(ctx)

	results := make(chan error, len(watches))

	var group sync.WaitGroup

	for _, watch := range watches {
		group.Go(func() {
			results <- WaitForNewMail(ctx, cfg, watch.Mailbox, watch.Seen, maxWait)
		})
	}

	defer func() {
		cancel()
		group.Wait()
	}()

	if len(watches) == 0 {
		return nil
	}

	return <-results
}

// idleUntil IDLEs until a message is reported on arrived, maxWait passes or
// ctx ends.
func (c *Client) idleUntil(
	ctx context.Context,
	mailbox string,
	arrived <-chan struct{},
	maxWait time.Duration,
) error {
	idle, err := c.imap.Idle()
	if err != nil {
		return fmt.Errorf("idle %s: %w", mailbox, err)
	}

	c.log.DebugContext(ctx, "idle", "mailbox", mailbox, "max_wait", maxWait)

	waitCtx, cancel := context.WithTimeout(ctx, maxWait)
	defer cancel()

	reason := "new mail"

	select {
	case <-arrived:
	case <-waitCtx.Done():
		reason = "timeout"
		if ctx.Err() != nil {
			reason = "stopped"
		}
	}

	stopCtx, stop := context.WithTimeout(context.WithoutCancel(ctx), idleStopTimeout)
	defer stop()

	defer c.CloseWhenDone(stopCtx)()

	_ = idle.Close()

	c.log.DebugContext(ctx, "idle ended", "mailbox", mailbox, "reason", reason)

	return nil
}

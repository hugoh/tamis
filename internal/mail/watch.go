package mail

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapclient"

	"github.com/hugoh/tamis/internal/retry"
)

const (
	// maxWatchRetry caps the wait between reconnection attempts.
	maxWatchRetry = 10 * time.Minute
	// loginRetry is the wait after a rejected login: long, so a wrong
	// password isn't tried often enough to lock the account, yet a provider's
	// passing trouble still heals without a restart.
	loginRetry = 15 * time.Minute
	// stableAfter is how long a connection must have stayed up for the
	// next failure to be retried quickly again.
	stableAfter = time.Minute
)

var errConnectionClosed = errors.New("connection closed")

// Watcher IDLEs on one long-lived connection per folder and reports when
// mail arrives or flags change. Each connection stays up for as long as the
// server allows, so a quiet mailbox costs no logins.
type Watcher struct {
	changed chan string
	idling  atomic.Int32
	cancel  context.CancelFunc
	group   sync.WaitGroup
}

// Watch starts watching mailboxes until ctx ends or Close is called. A folder
// whose connection fails is retried after about firstRetry, then ever more
// slowly while it keeps failing; one whose login is rejected is retried
// rarely; one that can't IDLE is given up on, since retrying won't help.
// Callers that also poll lose nothing by a folder dropping out.
func Watch(
	ctx context.Context,
	cfg DialConfig,
	mailboxes []string,
	firstRetry time.Duration,
) *Watcher {
	ctx, cancel := context.WithCancel(ctx)
	watcher := &Watcher{changed: make(chan string, 1), cancel: cancel}

	for _, mailbox := range mailboxes {
		watcher.group.Go(func() { watcher.keep(ctx, cfg, mailbox, firstRetry) })
	}

	return watcher
}

// Changed receives a reason whenever a watched folder changes. Changes
// that pile up before the receiver looks are reported once.
func (w *Watcher) Changed() <-chan string {
	return w.changed
}

// Idling counts the folders currently waiting in IDLE.
func (w *Watcher) Idling() int {
	return int(w.idling.Load())
}

// Close stops watching and waits for the connections to end.
func (w *Watcher) Close() {
	w.cancel()
	w.group.Wait()
}

func (w *Watcher) notify(reason string) {
	select {
	case w.changed <- reason:
	default:
	}
}

func (w *Watcher) keep(ctx context.Context, cfg DialConfig, mailbox string, firstRetry time.Duration) {
	log := cfg.Logger
	if log == nil {
		log = slog.New(slog.DiscardHandler)
	}

	pace := retry.Backoff(firstRetry, maxWatchRetry)

	for {
		started := time.Now()
		err := w.idleOn(ctx, cfg, mailbox)

		var wait time.Duration

		switch {
		case ctx.Err() != nil:
			return
		case errors.Is(err, ErrNoIdle):
			log.WarnContext(ctx, "server has no IDLE; not watching", "mailbox", mailbox)

			return
		case errors.Is(err, ErrLogin):
			wait = loginRetry

			log.ErrorContext(ctx, "login rejected; trying again later",
				"mailbox", mailbox, "err", err, "retry", wait)
		default:
			if time.Since(started) >= stableAfter {
				pace.Reset()
			}

			wait = pace.NextBackOff()

			log.WarnContext(ctx, "watch failed; retrying",
				"mailbox", mailbox, "err", err, "retry", wait)
		}

		select {
		case <-ctx.Done():
			return
		case <-time.After(wait):
		}
	}
}

// idleOn IDLEs on mailbox, reporting changes, until the connection fails or
// ctx ends.
func (w *Watcher) idleOn(ctx context.Context, cfg DialConfig, mailbox string) error {
	changed := make(chan string, 1)

	client, err := dial(ctx, cfg, idleHandler(changed))
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	if !client.imap.Caps().Has(imap.CapIdle) {
		return ErrNoIdle
	}

	if err := client.selectMailbox(mailbox); err != nil {
		return err
	}

	w.idling.Add(1)
	defer w.idling.Add(-1)

	for {
		reason, err := client.idleUntil(ctx, mailbox, changed)
		if err != nil || ctx.Err() != nil {
			return err
		}

		w.notify(reason)
	}
}

// idleHandler reports new mail, and the flag changes a server announces
// during IDLE such as a junk mark set in another client, on changed.
func idleHandler(changed chan<- string) *imapclient.UnilateralDataHandler {
	notify := func(reason string) {
		select {
		case changed <- reason:
		default:
		}
	}

	return &imapclient.UnilateralDataHandler{
		Mailbox: func(data *imapclient.UnilateralDataMailbox) {
			if data.NumMessages != nil {
				notify("new mail")
			}
		},
		Fetch: func(msg *imapclient.FetchMessageData) {
			_, _ = msg.Collect()

			notify("flags changed")
		},
	}
}

// idleUntil IDLEs until a change is reported on changed, the connection
// closes or ctx ends, and returns what ended it.
func (c *Client) idleUntil(
	ctx context.Context,
	mailbox string,
	changed <-chan string,
) (string, error) {
	idle, err := c.imap.Idle()
	if err != nil {
		return "", fmt.Errorf("idle %s: %w", mailbox, err)
	}

	c.log.DebugContext(ctx, "idle", "mailbox", mailbox)

	var reason string

	select {
	case reason = <-changed:
	case <-c.imap.Closed():
		reason = "connection closed"
	case <-ctx.Done():
		reason = "stopped"
	}

	stopCtx, stop := context.WithTimeout(context.WithoutCancel(ctx), idleStopTimeout)
	defer stop()

	defer c.CloseWhenDone(stopCtx)()

	closeErr := idle.Close()

	c.log.DebugContext(ctx, "idle ended", "mailbox", mailbox, "reason", reason)

	switch {
	case ctx.Err() != nil:
		return reason, nil
	case reason == "connection closed":
		return reason, fmt.Errorf("idle %s: %w", mailbox, errConnectionClosed)
	case closeErr != nil:
		return reason, fmt.Errorf("end idle %s: %w", mailbox, closeErr)
	}

	return reason, nil
}

// Package monitor reports to a dead-man's-switch service such as
// healthchecks.io: it pings when things work, says when they fail, and the
// service alerts when the pings stop. It never holds up the daemon.
package monitor

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"path"
	"sync"
	"time"

	"github.com/hugoh/tamis/internal/retry"
)

const (
	requestTimeout = 10 * time.Second
	attempts       = 3
	// maxBody keeps a ping's text well inside what services accept.
	maxBody = 10 << 10
	// resendWait caps the wait between tries of one ping.
	resendWait = 5 * time.Second
	// minGap spaces the success pings: a burst of mail makes a burst of passes.
	minGap = time.Minute
)

// Path suffixes of the ping URL.
const (
	okPath   = ""
	failPath = "fail"
)

var errStatus = errors.New("unexpected response")

// Pinger sends pings in the background. A nil *Pinger does nothing, so callers
// needn't check whether monitoring is configured.
type Pinger struct {
	base   *url.URL
	client *http.Client
	log    *slog.Logger
	now    func() time.Time

	mu      sync.Mutex
	pending *ping
	lastOK  time.Time
	wake    chan struct{}
	cancel  context.CancelFunc
	done    chan struct{}
}

type ping struct{ path, body string }

// New starts a Pinger for the check at rawURL, or returns nil when it is
// empty. It runs until Close or until ctx ends.
func New(ctx context.Context, rawURL string, log *slog.Logger) (*Pinger, error) {
	if rawURL == "" {
		return nil, nil //nolint:nilnil // no URL means no monitoring, and a nil Pinger is inert
	}

	base, err := url.Parse(rawURL)
	if err != nil {
		return nil, fmt.Errorf("monitor URL: %w", redact(err))
	}

	ctx, cancel := context.WithCancel(ctx)
	pinger := &Pinger{
		base:   base,
		client: &http.Client{Timeout: requestTimeout},
		log:    log,
		now:    time.Now,
		wake:   make(chan struct{}, 1),
		cancel: cancel,
		done:   make(chan struct{}),
	}

	go pinger.run(ctx)

	return pinger, nil
}

// Success says the daemon works. Pings come at most once a minute.
func (p *Pinger) Success(body string) {
	if p == nil {
		return
	}

	p.mu.Lock()
	defer p.mu.Unlock()

	if p.now().Sub(p.lastOK) < minGap {
		return
	}

	p.lastOK = p.now()
	p.queue(ping{okPath, body})
}

// Fail says something is wrong. The next Success is sent at once, so that the
// service sees the recovery.
func (p *Pinger) Fail(body string) {
	if p == nil {
		return
	}

	p.mu.Lock()
	defer p.mu.Unlock()

	p.lastOK = time.Time{}
	p.queue(ping{failPath, body})
}

// Test sends one success ping now and says whether the service took it.
func (p *Pinger) Test(ctx context.Context, body string) error {
	if p == nil {
		return nil
	}

	return p.deliver(ctx, ping{okPath, body})
}

// Close stops sending. A ping that hasn't gone out is dropped: stopping the
// daemon is something the service should alert on.
func (p *Pinger) Close() {
	if p == nil {
		return
	}

	p.cancel()
	<-p.done
}

// queue keeps only the newest ping that hasn't gone out: it is the truth now.
func (p *Pinger) queue(next ping) {
	p.pending = &next

	select {
	case p.wake <- struct{}{}:
	default:
	}
}

func (p *Pinger) run(ctx context.Context) {
	defer close(p.done)

	var streak retry.Streak

	for {
		select {
		case <-ctx.Done():
			return
		case <-p.wake:
		}

		p.mu.Lock()
		next := p.pending
		p.pending = nil
		p.mu.Unlock()

		if next != nil {
			p.report(ctx, &streak, p.send(ctx, *next))
		}
	}
}

// report logs delivery trouble the way other trouble is logged: when it
// starts, now and then while it lasts, and when it ends.
func (p *Pinger) report(ctx context.Context, streak *retry.Streak, err error) {
	if err == nil {
		if failures, downtime, ok := streak.Recover(time.Now()); ok {
			p.log.InfoContext(ctx, "monitoring ping delivered again",
				"failures", failures, "downtime", downtime)
		}

		return
	}

	if ctx.Err() != nil {
		return
	}

	failures := streak.Fail(time.Now())
	p.log.Log(ctx, retry.Level(failures, slog.LevelWarn), "monitoring ping failed",
		"host", p.base.Host, "err", err, "failures", failures)
}

// send delivers a ping, trying again briefly when the service or the network
// hiccups.
func (p *Pinger) send(ctx context.Context, next ping) error {
	pace := retry.Backoff(time.Second, resendWait)

	var err error

	for try := range attempts {
		if try > 0 {
			select {
			case <-ctx.Done():
				return ctx.Err() //nolint:wrapcheck // shutting down
			case <-time.After(pace.NextBackOff()):
			}
		}

		if err = p.deliver(ctx, next); err == nil || !retryable(err) {
			return err
		}
	}

	return err
}

type statusError struct{ code int }

func (e statusError) Error() string { return fmt.Sprintf("%v: HTTP %d", errStatus, e.code) }

func (e statusError) Unwrap() error { return errStatus }

// retryable is a network failure, or a service that is overloaded or broken.
func retryable(err error) bool {
	if status, ok := errors.AsType[statusError](err); ok {
		return status.code == http.StatusTooManyRequests ||
			status.code >= http.StatusInternalServerError
	}

	return true
}

func (p *Pinger) deliver(ctx context.Context, next ping) error {
	target := *p.base
	target.Path = path.Join(target.Path, next.path)

	body := next.body
	if len(body) > maxBody {
		body = body[:maxBody]
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target.String(),
		bytes.NewBufferString(body))
	if err != nil {
		return fmt.Errorf("build ping: %w", redact(err))
	}

	req.Header.Set("Content-Type", "text/plain; charset=utf-8")

	resp, err := p.client.Do(req)
	if err != nil {
		return fmt.Errorf("ping: %w", redact(err))
	}

	defer func() { _ = resp.Body.Close() }()

	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, maxBody))

	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return statusError{resp.StatusCode}
	}

	return nil
}

// redact drops the URL from a network error: it holds the check's secret.
func redact(err error) error {
	if urlErr, ok := errors.AsType[*url.Error](err); ok {
		return urlErr.Err
	}

	return err
}

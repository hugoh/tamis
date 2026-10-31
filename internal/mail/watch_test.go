package mail_test

import (
	"bytes"
	"log/slog"
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/require"
)

const (
	watchWait  = 5 * time.Second
	watchRetry = 10 * time.Millisecond
	work       = "Work"
)

type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()

	return b.buf.Write(p) //nolint:wrapcheck // test double
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()

	return b.buf.String()
}

func watchConfig(srv *imaptest.Server) (mail.DialConfig, *syncBuffer) {
	logs := &syncBuffer{}
	cfg := dialConfig(srv)
	cfg.Logger = slog.New(slog.NewTextHandler(logs, &slog.HandlerOptions{Level: slog.LevelDebug}))

	return cfg, logs
}

func watchIdling(t *testing.T, w *mail.Watcher, folders int) {
	t.Helper()
	require.Eventually(t, func() bool { return w.Idling() == folders }, watchWait, watchRetry)
}

func hears(t *testing.T, w *mail.Watcher) {
	t.Helper()

	select {
	case <-w.Changed():
	case <-time.After(watchWait):
		require.FailNow(t, "the watcher heard nothing")
	}
}

func TestWatcherReportsChanges(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		folders []string
		setup   func(t *testing.T, client *mail.Client) []uint32
		act     func(t *testing.T, client *mail.Client, uids []uint32)
	}{
		{
			name:    "message arrives",
			folders: []string{inboxName},
			act: func(t *testing.T, client *mail.Client, _ []uint32) {
				t.Helper()
				seed(t, client, inboxName, raw("a@x", "b@y", "pushed"))
			},
		},
		{
			name:    "message is marked",
			folders: []string{inboxName},
			setup: func(t *testing.T, client *mail.Client) []uint32 {
				t.Helper()

				return seed(t, client, inboxName, raw("a@x", "b@y", "marked later"))
			},
			act: func(t *testing.T, client *mail.Client, uids []uint32) {
				t.Helper()
				require.NoError(t, client.AddKeyword(inboxName, uids, "Junk"))
			},
		},
		{
			name:    "message arrives in the second folder",
			folders: []string{inboxName, work},
			act: func(t *testing.T, client *mail.Client, _ []uint32) {
				t.Helper()
				seed(t, client, work, raw("a@x", "b@y", "pushed"))
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())
			client := connect(t, srv)

			for _, folder := range tt.folders {
				require.NoError(t, client.Ensure(folder))
			}

			var uids []uint32
			if tt.setup != nil {
				uids = tt.setup(t, client)
			}

			w := mail.Watch(t.Context(), dialConfig(srv), tt.folders, watchRetry)
			t.Cleanup(w.Close)
			watchIdling(t, w, len(tt.folders))

			tt.act(t, client, uids)
			hears(t, w)
		})
	}
}

func TestWatcherIsQuietWhenNothingArrives(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())
	w := mail.Watch(t.Context(), dialConfig(srv), []string{inboxName}, watchRetry)
	t.Cleanup(w.Close)
	watchIdling(t, w, 1)

	select {
	case reason := <-w.Changed():
		require.FailNow(t, "unexpected change", reason)
	case <-time.After(300 * time.Millisecond):
	}
}

func TestWatcherKeepsOneConnection(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())
	client := connect(t, srv)
	cfg, logs := watchConfig(srv)

	w := mail.Watch(t.Context(), cfg, []string{inboxName}, watchRetry)
	t.Cleanup(w.Close)
	watchIdling(t, w, 1)

	for _, subject := range []string{"one", "two", "three"} {
		seed(t, client, inboxName, raw("a@x", "b@y", subject))
		hears(t, w)
	}

	require.Equal(t, 1, strings.Count(logs.String(), "msg=connected"))
	require.Equal(t, 1, strings.Count(logs.String(), "msg=select"))
}

func TestWatcherReconnectsAfterAFailure(t *testing.T) {
	t.Parallel()

	var failing atomic.Bool

	failing.Store(true)

	srv := imaptest.StartWith(t, imaptest.Options{
		Caps: imaptest.Rev2(),
		Fail: func(op, _ string) bool { return failing.Load() && op == "SELECT" },
	})
	client := connect(t, srv)
	cfg, logs := watchConfig(srv)

	w := mail.Watch(t.Context(), cfg, []string{inboxName}, watchRetry)
	t.Cleanup(w.Close)

	require.Eventually(t, func() bool {
		return strings.Contains(logs.String(), "retrying")
	}, watchWait, watchRetry)

	failing.Store(false)
	watchIdling(t, w, 1)

	seed(t, client, inboxName, raw("a@x", "b@y", "after the outage"))
	hears(t, w)
}

func TestWatcherGivesUp(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		// serve returns the address to watch and what to credit the dial with.
		start func(t *testing.T) mail.DialConfig
		want  string
	}{
		{
			name: "server without IDLE",
			start: func(t *testing.T) mail.DialConfig {
				t.Helper()

				listener, err := (&net.ListenConfig{}).Listen(t.Context(), "tcp", "127.0.0.1:0")
				require.NoError(t, err)
				t.Cleanup(func() { _ = listener.Close() })

				go serveWithoutIdle(listener)

				return mail.DialConfig{
					Addr:      listener.Addr().String(),
					Username:  imaptest.Username,
					Password:  imaptest.Password,
					Plaintext: true,
				}
			},
			want: "no IDLE",
		},
		{
			name: "rejected login",
			start: func(t *testing.T) mail.DialConfig {
				t.Helper()

				cfg := dialConfig(imaptest.Start(t, imaptest.Rev2()))
				cfg.Password = "wrong"

				return cfg
			},
			want: "login rejected",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			logs := &syncBuffer{}
			cfg := tt.start(t)
			cfg.Logger = slog.New(slog.NewTextHandler(logs, nil))

			w := mail.Watch(t.Context(), cfg, []string{inboxName}, watchRetry)
			t.Cleanup(w.Close)

			require.Eventually(t, func() bool {
				return strings.Contains(logs.String(), tt.want)
			}, watchWait, watchRetry)

			time.Sleep(20 * watchRetry)

			require.NotContains(t, logs.String(), "retrying", "retrying can't help")
			require.Zero(t, w.Idling())
		})
	}
}

func TestWatcherStops(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())
	w := mail.Watch(t.Context(), dialConfig(srv), []string{inboxName, "Missing"}, time.Hour)
	watchIdling(t, w, 1)

	done := make(chan struct{})

	go func() {
		w.Close()
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(watchWait):
		require.FailNow(t, "Close did not return")
	}

	require.Zero(t, w.Idling())
}

func TestWatcherWithoutFoldersIsInert(t *testing.T) {
	t.Parallel()

	w := mail.Watch(t.Context(), mail.DialConfig{}, nil, time.Hour)
	require.Zero(t, w.Idling())
	w.Close()
}

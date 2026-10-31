package web_test

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"sync"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

func get(t *testing.T, url string) (*http.Response, error) {
	t.Helper()

	req, err := http.NewRequestWithContext(t.Context(), http.MethodGet, url, nil)
	require.NoError(t, err)

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("get %s: %w", url, err)
	}

	return resp, nil
}

func TestServeAndShutDown(t *testing.T) {
	t.Parallel()

	var lc net.ListenConfig

	listener, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	done := make(chan error, 1)
	srv := newHarness(t).srv

	go func() { done <- srv.Serve(ctx, listener) }()

	url := "http://" + listener.Addr().String() + loginPath

	resp, err := get(t, url)
	require.NoError(t, err)
	require.NoError(t, resp.Body.Close())
	require.Equal(t, http.StatusOK, resp.StatusCode)

	cancel()

	select {
	case err := <-done:
		require.NoError(t, err, "a requested shutdown is not an error")
	case <-time.After(5 * time.Second):
		t.Fatal("server did not shut down")
	}

	if resp, err := get(t, url); err == nil {
		_ = resp.Body.Close()

		t.Fatal("server still answers after shutdown")
	}
}

// recorder is a slog handler that keeps what it is given.
type recorder struct {
	mu      sync.Mutex
	records []slog.Record
}

func (r *recorder) Enabled(context.Context, slog.Level) bool { return true }

func (r *recorder) Handle(_ context.Context, record slog.Record) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.records = append(r.records, record)

	return nil
}

func (r *recorder) WithAttrs([]slog.Attr) slog.Handler { return r }
func (r *recorder) WithGroup(string) slog.Handler      { return r }

func (r *recorder) levels() []slog.Level {
	r.mu.Lock()
	defer r.mu.Unlock()

	levels := make([]slog.Level, len(r.records))
	for i, record := range r.records {
		levels[i] = record.Level
	}

	return levels
}

func TestAClientThatHangsUpBeforeTLSIsOnlyDebugNoise(t *testing.T) {
	t.Parallel()

	harness := newHarness(t)
	logs := &recorder{}

	srv, err := web.New(testConfig(), harness.db, slog.New(logs), web.Options{})
	require.NoError(t, err)

	certFile, keyFile, err := web.SelfSigned(t.TempDir(), tlsHost, time.Now())
	require.NoError(t, err)

	var lc net.ListenConfig

	plain, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)

	listener, err := web.TLSListener(plain, certFile, keyFile)
	require.NoError(t, err)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	go func() { _ = srv.Serve(ctx, listener) }()

	var dialer net.Dialer

	conn, err := dialer.DialContext(t.Context(), "tcp", plain.Addr().String())
	require.NoError(t, err)
	require.NoError(t, conn.Close())

	require.Eventually(
		t,
		func() bool { return len(logs.levels()) > 0 },
		5*time.Second,
		10*time.Millisecond,
	)

	for _, level := range logs.levels() {
		require.Equal(
			t,
			slog.LevelDebug,
			level,
			"a port check or a browser giving up is not a problem",
		)
	}
}

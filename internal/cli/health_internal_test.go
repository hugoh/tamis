package cli

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/monitor"
	"github.com/hugoh/tamis/internal/retry"
	"github.com/stretchr/testify/require"
)

const warnPass = "WARN pass"

var errDown = errors.New("connection refused")

func newHealth() (*passHealth, *bytes.Buffer, *slog.Logger) {
	out := new(bytes.Buffer)
	logger := slog.New(slog.NewTextHandler(out, &slog.HandlerOptions{Level: slog.LevelDebug}))

	return &passHealth{pace: retry.Backoff(time.Second, time.Minute)}, out, logger
}

func levelsOf(out string) []string {
	var levels []string

	for line := range strings.Lines(out) {
		_, rest, _ := strings.Cut(line, "level=")
		level, _, _ := strings.Cut(rest, " ")
		message := strings.TrimPrefix(strings.SplitN(rest, "msg=", 2)[1], `"`)
		levels = append(levels, level+" "+strings.Fields(message)[0])
	}

	return levels
}

func TestPassHealthLogsTheStartRemindersAndTheEnd(t *testing.T) {
	t.Parallel()

	health, out, logger := newHealth()

	for range 6 {
		require.Positive(t, health.after(t.Context(), logger, errDown))
	}

	require.Zero(t, health.after(t.Context(), logger, nil))
	require.Zero(t, health.after(t.Context(), logger, nil), "success is quiet once recovered")

	require.Equal(t, []string{
		"ERROR pass", // the first failure
		warnPass,     // 2nd: still failing
		warnPass,     // 3rd
		"DEBUG pass", // 4th: routine
		warnPass,     // 5th
		"DEBUG pass", // 6th
		"INFO pass",  // recovered
	}, levelsOf(out.String()))
	require.Contains(t, out.String(), "msg=\"pass recovered\" failures=6")
}

func TestPassHealthWaitsLongerAfterEachFailure(t *testing.T) {
	t.Parallel()

	health, _, logger := newHealth()

	first := health.after(t.Context(), logger, errDown)

	var last time.Duration
	for range 5 {
		last = health.after(t.Context(), logger, errDown)
	}

	require.Greater(t, last, 2*first)

	health.after(t.Context(), logger, nil)
	require.Less(t, health.after(t.Context(), logger, errDown), 2*first, "a success starts over")
}

func TestPassHealthReportsARejectedLoginOnceThenHourly(t *testing.T) {
	t.Parallel()

	health, out, logger := newHealth()
	rejected := fmt.Errorf("pass: %w", mail.ErrLogin)

	for range 8 {
		require.Equal(t, loginRetry, health.after(t.Context(), logger, rejected))
	}

	health.after(t.Context(), logger, nil)

	require.Equal(t, 1, strings.Count(out.String(), "level=ERROR"))
	require.Equal(t, 2, strings.Count(out.String(), "level=WARN"), "the 4th and 8th")
	require.Contains(t, out.String(), "msg=\"login accepted again\" failures=8")
}

const (
	beatPing = "/ping/key"
	failPing = "/ping/key/fail"
)

type heard struct {
	mu    sync.Mutex
	paths []string
}

func (h *heard) add(path string) {
	h.mu.Lock()
	defer h.mu.Unlock()

	h.paths = append(h.paths, path)
}

func (h *heard) all() []string {
	h.mu.Lock()
	defer h.mu.Unlock()

	return append([]string(nil), h.paths...)
}

func monitored(t *testing.T, failAfter int) (*passHealth, *heard, *slog.Logger) {
	t.Helper()

	got := &heard{}
	srv := httptest.NewServer(http.HandlerFunc(func(resp http.ResponseWriter, req *http.Request) {
		got.add(req.URL.Path)
		resp.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(srv.Close)

	logger := slog.New(slog.DiscardHandler)
	beat, err := monitor.New(t.Context(), srv.URL+"/ping/key", logger)
	require.NoError(t, err)
	t.Cleanup(beat.Close)

	health, _, _ := newHealth()
	health.beat, health.failAfter, health.status = beat, failAfter, live.NewStatus(time.Now())

	return health, got, logger
}

func eventually(t *testing.T, got *heard, want []string) {
	t.Helper()

	require.Eventually(t, func() bool { return slices.Equal(got.all(), want) },
		2*time.Second, 5*time.Millisecond, "heard %v", got.all())
}

func TestPassHealthPingsTheMonitorOnlyOnceFailuresPersist(t *testing.T) {
	t.Parallel()

	health, got, logger := monitored(t, 3)

	health.after(t.Context(), logger, nil)
	eventually(t, got, []string{beatPing})

	health.after(t.Context(), logger, errDown)
	health.after(t.Context(), logger, errDown)

	time.Sleep(50 * time.Millisecond)
	require.Equal(t, []string{beatPing}, got.all(), "two failures are left to the grace time")

	health.after(t.Context(), logger, errDown)
	eventually(t, got, []string{beatPing, failPing})

	health.after(t.Context(), logger, nil)
	eventually(t, got, []string{beatPing, failPing, beatPing})
}

func TestPassHealthTellsTheMonitorOfARejectedLoginAtOnce(t *testing.T) {
	t.Parallel()

	health, got, logger := monitored(t, 3)

	health.after(t.Context(), logger, fmt.Errorf("pass: %w", mail.ErrLogin))
	eventually(t, got, []string{failPing})
}

package monitor_test

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/monitor"
	"github.com/stretchr/testify/require"
)

const checkID = "heartbeat-check"

type received struct {
	path, body string
}

type service struct {
	srv  *httptest.Server
	mu   sync.Mutex
	seen []received
}

func newService(t *testing.T, status func(n int) int) *service {
	t.Helper()

	svc := &service{}

	var calls atomic.Int32

	svc.srv = httptest.NewServer(
		http.HandlerFunc(func(resp http.ResponseWriter, req *http.Request) {
			body, _ := io.ReadAll(req.Body)

			svc.mu.Lock()
			svc.seen = append(svc.seen, received{req.URL.Path, string(body)})
			svc.mu.Unlock()

			resp.WriteHeader(status(int(calls.Add(1))))
		}),
	)
	t.Cleanup(svc.srv.Close)

	return svc
}

func (s *service) url() string { return s.srv.URL + "/ping/" + checkID }

func (s *service) got() []received {
	s.mu.Lock()
	defer s.mu.Unlock()

	return append([]received(nil), s.seen...)
}

func ok(int) int { return http.StatusOK }

// syncLog is a log sink the test can read while the pinger writes.
type syncLog struct {
	mu   sync.Mutex
	text strings.Builder
}

func (l *syncLog) Write(data []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()

	return l.text.Write(data) //nolint:wrapcheck // a test sink
}

func (l *syncLog) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()

	return l.text.String()
}

func start(t *testing.T, rawURL string, logs *syncLog) *monitor.Pinger {
	t.Helper()

	sink := io.Discard
	if logs != nil {
		sink = logs
	}

	pinger, err := monitor.New(
		t.Context(), rawURL, slog.New(slog.NewTextHandler(sink, nil)),
	)
	require.NoError(t, err)
	t.Cleanup(pinger.Close)

	return pinger
}

func TestNoURLMeansNoMonitoring(t *testing.T) {
	t.Parallel()

	pinger, err := monitor.New(context.Background(), "", slog.New(slog.DiscardHandler))
	require.NoError(t, err)
	require.Nil(t, pinger)

	pinger.Success("x") // a nil Pinger is inert
	pinger.Fail("x")
	pinger.Close()
	require.NoError(t, pinger.Test(t.Context(), "x"))
}

func TestSuccessAndFailGoToTheirOwnPaths(t *testing.T) {
	t.Parallel()

	svc := newService(t, ok)
	pinger := start(t, svc.url(), nil)

	pinger.Success("scanned 2")
	require.Eventually(
		t,
		func() bool { return len(svc.got()) == 1 },
		2*time.Second,
		5*time.Millisecond,
	)

	pinger.Fail("3 passes failed")
	require.Eventually(
		t,
		func() bool { return len(svc.got()) == 2 },
		2*time.Second,
		5*time.Millisecond,
	)

	require.Equal(t, []received{
		{"/ping/" + checkID, "scanned 2"},
		{"/ping/" + checkID + "/fail", "3 passes failed"},
	}, svc.got())
}

func TestSuccessPingsAreSpacedButRecoveryIsNot(t *testing.T) {
	t.Parallel()

	svc := newService(t, ok)
	pinger := start(t, svc.url(), nil)

	pinger.Success("one")
	pinger.Success("two") // within a minute: dropped
	pinger.Success("three")
	require.Eventually(
		t,
		func() bool { return len(svc.got()) >= 1 },
		2*time.Second,
		5*time.Millisecond,
	)

	pinger.Fail("down")
	require.Eventually(
		t,
		func() bool { return len(svc.got()) == 2 },
		2*time.Second,
		5*time.Millisecond,
	)

	pinger.Success("back") // right after a failure: sent, so the service sees the recovery
	require.Eventually(
		t,
		func() bool { return len(svc.got()) == 3 },
		2*time.Second,
		5*time.Millisecond,
	)
	require.Equal(t, "back", svc.got()[2].body)
}

func TestServerTroubleIsRetried(t *testing.T) {
	t.Parallel()

	svc := newService(t, func(n int) int {
		if n == 1 {
			return http.StatusBadGateway
		}

		return http.StatusOK
	})
	pinger := start(t, svc.url(), nil)

	pinger.Success("x")
	require.Eventually(
		t,
		func() bool { return len(svc.got()) == 2 },
		10*time.Second,
		10*time.Millisecond,
	)
}

func TestWrongURLIsNotRetriedAndNeverLogsTheSecret(t *testing.T) {
	t.Parallel()

	svc := newService(t, func(int) int { return http.StatusNotFound })

	logs := &syncLog{}
	pinger := start(t, svc.url(), logs)

	pinger.Success("x")
	require.Eventually(
		t,
		func() bool { return strings.Contains(logs.String(), "monitoring ping failed") },
		2*time.Second,
		5*time.Millisecond,
	)
	require.Len(t, svc.got(), 1, "a 404 won't get better")
	require.NotContains(t, logs.String(), checkID)
}

func TestUnreachableServiceNeverLogsTheSecret(t *testing.T) {
	t.Parallel()

	svc := newService(t, ok)
	target := svc.url()
	svc.srv.Close()

	pinger := start(t, target, nil)
	err := pinger.Test(t.Context(), "x")
	require.Error(t, err)
	require.NotContains(t, err.Error(), checkID)
}

func TestTestReportsWhatTheServiceSaid(t *testing.T) {
	t.Parallel()

	svc := newService(t, func(int) int { return http.StatusForbidden })
	pinger := start(t, svc.url(), nil)

	err := pinger.Test(t.Context(), "tamis monitor test")
	require.ErrorContains(t, err, "HTTP 403")
}

func TestRecommendForTheDefaults(t *testing.T) {
	t.Parallel()

	settings := monitor.Recommend(monitor.Inputs{
		Poll:        2 * time.Minute,
		FailAfter:   3,
		PassTimeout: 10 * time.Minute,
		MaxRetry:    15 * time.Minute,
	})

	require.Equal(
		t,
		monitor.Check{Name: "tamis heartbeat", Timeout: 120, Grace: 660},
		settings.Heartbeat,
	)
	require.Equal(
		t,
		monitor.Check{Name: "tamis quality", Timeout: 86400, Grace: 7200},
		settings.Quality,
	)

	after := map[string]int{}
	for _, alert := range settings.Alerts {
		after[alert.When] = alert.AfterSeconds
	}

	require.Equal(t, 780, after["the daemon stops, hangs or its host goes down"], "13 minutes")
	require.Equal(t, 360, after["the mail server is unreachable (fail_after = 3)"], "2 + 4 minutes")
	require.Zero(t, after["the login is rejected"])
}

func TestRecommendKeepsTheShortestPeriod(t *testing.T) {
	t.Parallel()

	settings := monitor.Recommend(monitor.Inputs{
		Poll: 10 * time.Second, FailAfter: 1, PassTimeout: time.Minute, MaxRetry: time.Hour,
	})
	require.Equal(t, 60, settings.Heartbeat.Timeout)
	require.Zero(t, settings.Alerts[1].AfterSeconds, "the first failure alerts at once")
}

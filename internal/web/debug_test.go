package web_test

import (
	"log/slog"
	"net/http"
	"net/url"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	logsPath      = "/logs"
	debugPath     = "/logs/debug"
	debugOffLabel = "Debug logging off"
)

func debugBrowser(t *testing.T, base slog.Level) (*harness, *live.DebugSwitch) {
	t.Helper()

	debug := live.NewDebugSwitch(base, 15*time.Minute)
	browser := newHarnessWith(t, testConfig(), func(options *web.Options) { options.Debug = debug })
	browser.enroll(browser.token())

	return browser, debug
}

func TestLogsPageOffersTheDebugSwitch(t *testing.T) {
	t.Parallel()

	browser, _ := debugBrowser(t, slog.LevelInfo)

	body := browser.do(http.MethodGet, logsPath, nil).Body.String()
	require.Contains(t, body, debugOffLabel)
	require.Contains(t, body, `hx-post="/logs/debug?state=on"`)
	require.Contains(t, body, "15 minutes")
}

func TestDebugCanBeTurnedOnAndOff(t *testing.T) {
	t.Parallel()

	browser, debug := debugBrowser(t, slog.LevelInfo)

	rec := browser.form(debugPath+"?state=on", url.Values{})
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "Debug logging on until")
	require.Contains(t, rec.Body.String(), `hx-post="/logs/debug?state=off"`)

	on, _ := debug.State()
	require.True(t, on)
	require.Equal(t, slog.LevelDebug, debug.Level().Level())

	rec = browser.form(debugPath+"?state=off", url.Values{})
	require.Contains(t, rec.Body.String(), debugOffLabel)

	on, _ = debug.State()
	require.False(t, on)
}

func TestTheLogsPageShowsDebugTurningItselfOff(t *testing.T) {
	t.Parallel()

	browser, debug := debugBrowser(t, slog.LevelInfo)
	debug.On()
	require.Contains(
		t,
		browser.do(http.MethodGet, logsPath, nil).Body.String(),
		"Debug logging on until",
	)

	debug.Off()
	require.Contains(t, browser.do(http.MethodGet, logsPath, nil).Body.String(), debugOffLabel)
}

func TestDebugSwitchNeedsASessionAndAnOrigin(t *testing.T) {
	t.Parallel()

	anonymous := newHarnessWith(t, testConfig(), func(options *web.Options) {
		options.Debug = live.NewDebugSwitch(slog.LevelInfo, time.Minute)
	})

	rec := anonymous.form(debugPath+"?state=on", url.Values{})
	require.Equal(t, http.StatusUnauthorized, rec.Code)

	browser, debug := debugBrowser(t, slog.LevelInfo)

	rec = browser.do(http.MethodPost, debugPath+"?state=on", nil, "Origin", "https://evil.example")
	require.Equal(t, http.StatusForbidden, rec.Code)

	on, _ := debug.State()
	require.False(t, on)
}

func TestVerboseDaemonShowsDebugAsAlwaysOn(t *testing.T) {
	t.Parallel()

	browser, _ := debugBrowser(t, slog.LevelDebug)

	body := browser.do(http.MethodGet, logsPath, nil).Body.String()
	require.Contains(t, body, "--verbose")
	require.NotContains(t, body, "/logs/debug")
}

func TestNoSwitchWhenTheServerHasNone(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	body := browser.do(http.MethodGet, logsPath, nil).Body.String()
	require.NotContains(t, body, "Debug logging")
	require.Equal(t, http.StatusNotFound, browser.form(debugPath+"?state=on", url.Values{}).Code)
}

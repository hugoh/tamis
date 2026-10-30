package web_test

import (
	"bytes"
	"log/slog"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/store"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	whitelistPath = "/whitelist"
	removePath    = "/whitelist/remove"
	undoPath      = "/decisions/1/undo"
	inbox         = "INBOX"
	bayes         = "bayes"
	spamSubject   = "WINNER claim prize"
	goodSubject   = "Lunch on Thursday"
)

func (h *harness) record(decision store.Decision) {
	h.t.Helper()

	decision.At = time.Now()
	_, err := h.db.RecordDecision(h.t.Context(), decision)
	require.NoError(h.t, err)
}

// signedIn is a harness with decisions and addresses to look at.
func signedIn(t *testing.T) *harness {
	t.Helper()

	browser := newHarness(t)
	browser.record(store.Decision{
		Mailbox: inbox,
		From:    "spammer@example.net",
		Subject: spamSubject,
		Score:   0.97,
		Verdict: engine.VerdictSpam,
		Reason:  "bayes",
		Action:  "move:Junk",
		Applied: true,
	})
	browser.record(store.Decision{
		Mailbox: inbox, From: "friend@example.com", Subject: goodSubject,
		Score: 0.05, Verdict: engine.VerdictGood, Reason: bayes, Action: "move:Junk",
	})
	browser.record(store.Decision{
		Mailbox: inbox, From: "evil@example.net", Subject: "<script>alert(1)</script>",
		Score: 0.5, Verdict: engine.VerdictUncertain, Reason: bayes, Action: "none",
	})
	require.NoError(t, browser.db.AddAddress(t.Context(), "Friend@Example.com", "manual"))
	browser.enroll(browser.token())

	return browser
}

func TestPagesNeedLogin(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)

	for _, path := range []string{"/", "/health", whitelistPath} {
		rec := browser.do(http.MethodGet, path, nil)
		require.Equal(t, http.StatusSeeOther, rec.Code, path)
	}
}

func TestDecisionsPage(t *testing.T) {
	t.Parallel()

	rec := signedIn(t).do(http.MethodGet, "/", nil)
	require.Equal(t, http.StatusOK, rec.Code)

	body := rec.Body.String()
	require.Contains(t, body, spamSubject)
	require.Contains(t, body, "spammer@example.net")
	require.Contains(t, body, "0.97")
	require.Contains(t, body, ">move:Junk<", "an applied action is shown as done")
	require.Contains(t, body, "would move:Junk", "a shadow-mode action says so")
	require.Contains(t, body, `href="/health"`)
	require.Contains(t, body, "/static/vendor/pico.min.css")
	require.Contains(t, body, "/static/vendor/htmx.min.js")
}

func TestDecisionsAreEscaped(t *testing.T) {
	t.Parallel()

	body := signedIn(t).do(http.MethodGet, "/", nil).Body.String()
	require.NotContains(t, body, "<script>alert(1)")
	require.Contains(t, body, "&lt;script&gt;alert(1)")
}

func TestDecisionsFilter(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		query         string
		shown, hidden []string
	}{
		"spam only":   {"?verdict=spam", []string{spamSubject}, []string{goodSubject}},
		"good only":   {"?verdict=good", []string{goodSubject}, []string{spamSubject}},
		"unknown":     {"?verdict=bogus", []string{spamSubject, goodSubject}, nil},
		"limit":       {"?n=1", []string{"<script>"}, []string{spamSubject}},
		"bad limit":   {"?n=lots", []string{spamSubject, goodSubject}, nil},
		"negative n":  {"?n=-4", []string{spamSubject, goodSubject}, nil},
		"huge number": {"?n=99999999", []string{spamSubject, goodSubject}, nil},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := signedIn(t).do(http.MethodGet, "/"+tt.query, nil)
			require.Equal(t, http.StatusOK, rec.Code)

			for _, want := range tt.shown {
				if want == "<script>" {
					want = "&lt;script&gt;"
				}

				require.Contains(t, rec.Body.String(), want)
			}

			for _, unwanted := range tt.hidden {
				require.NotContains(t, rec.Body.String(), unwanted)
			}
		})
	}
}

func TestHtmxGetsOnlyTheTable(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	full := browser.do(http.MethodGet, "/?verdict=spam", nil)
	require.Contains(t, full.Body.String(), "<html")

	part := browser.do(http.MethodGet, "/?verdict=spam", nil, "HX-Request", "true")
	require.Equal(t, http.StatusOK, part.Code)
	require.NotContains(t, part.Body.String(), "<html")
	require.Contains(t, part.Body.String(), spamSubject)
	require.NotContains(t, part.Body.String(), goodSubject)
}

func TestHealthPage(t *testing.T) {
	t.Parallel()

	rec := signedIn(t).do(http.MethodGet, "/health", nil)
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "class balance")
	require.Contains(t, rec.Body.String(), "last decision")
}

func TestWhitelistPage(t *testing.T) {
	t.Parallel()

	rec := signedIn(t).do(http.MethodGet, whitelistPath, nil)
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "friend@example.com")
	require.Contains(t, rec.Body.String(), "manual")
}

func TestVendoredAssets(t *testing.T) {
	t.Parallel()

	tests := map[string]string{
		"/static/vendor/pico.min.css": "text/css",
		"/static/vendor/htmx.min.js":  "text/javascript",
		"/static/app.css":             "text/css",
	}

	for path, contentType := range tests {
		t.Run(path, func(t *testing.T) {
			t.Parallel()

			rec := newHarness(t).do(http.MethodGet, path, nil)
			require.Equal(t, http.StatusOK, rec.Code)
			require.Contains(t, rec.Header().Get("Content-Type"), contentType)
		})
	}
}

func TestDatabaseFailureIsA500(t *testing.T) {
	t.Parallel()

	tests := map[string]struct{ method, path string }{
		"decisions":        {http.MethodGet, "/"},
		"health":           {http.MethodGet, "/health"},
		"whitelist":        {http.MethodGet, whitelistPath},
		"whitelist add":    {http.MethodPost, whitelistPath},
		"whitelist remove": {http.MethodPost, removePath},
		"undo":             {http.MethodPost, undoPath},
		"learn":            {http.MethodPost, "/decisions/2/learn"},
		"enroll":           {http.MethodPost, registerPath + "/begin?token=x"},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			browser := signedIn(t)
			if name == "enroll" {
				browser = browser.fork()
			}

			require.NoError(t, browser.db.Close())

			rec := browser.do(tt.method, tt.path, []byte("address=a%40example.com"),
				"Content-Type", formType, htmxHeader, "true")
			require.Equal(t, http.StatusInternalServerError, rec.Code)
			require.NotContains(t, rec.Body.String(), "sql", "internals stay in the log")
		})
	}
}

func TestLogsPage(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	logger := slog.New(browser.logs.Handler(slog.NewTextHandler(new(bytes.Buffer), nil)))
	logger.Info("pass complete", "scanned", 3)
	logger.Error("pass failed", "err", "<b>boom</b>")

	rec := browser.do(http.MethodGet, "/logs", nil)
	require.Equal(t, http.StatusOK, rec.Code)

	body := rec.Body.String()
	require.Contains(t, body, "pass complete scanned=3")
	require.Contains(t, body, "ERROR")
	require.Less(
		t,
		strings.Index(body, "pass failed"),
		strings.Index(body, "pass complete"),
		"newest first",
	)
	require.Contains(t, body, "&lt;b&gt;boom&lt;/b&gt;", "log text is escaped")
	require.NotContains(t, body, "<b>boom</b>")
	require.Contains(t, body, `hx-trigger="every 10s"`)
	require.Contains(t, body, `href="/logs"`)

	part := browser.do(http.MethodGet, "/logs", nil, htmxHeader, "true")
	require.NotContains(t, part.Body.String(), "<html")
	require.Contains(t, part.Body.String(), "pass failed")
}

func TestLogsPageWhenEmpty(t *testing.T) {
	t.Parallel()

	rec := signedIn(t).do(http.MethodGet, "/logs", nil)
	require.Contains(t, rec.Body.String(), "No log lines yet")
}

func TestLogsNeedLogin(t *testing.T) {
	t.Parallel()

	require.Equal(t, http.StatusSeeOther, newHarness(t).do(http.MethodGet, "/logs", nil).Code)
}

func TestHealthShowsTheDaemon(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	body := browser.do(http.MethodGet, "/health", nil).Body.String()
	require.Contains(t, body, "Daemon")
	require.Contains(t, body, "no pass yet")

	browser.status.Wait(live.Running)
	browser.status.PassDone(time.Now(), "scanned 3, spam 1")
	browser.status.Failed(time.Now(), errGone)
	browser.status.Wait(live.Idle)

	body = browser.do(http.MethodGet, "/health", nil).Body.String()
	require.Contains(t, body, "scanned 3, spam 1")
	require.Contains(t, body, "idle")
	require.Contains(t, body, "message not found where tamis left it")
	require.Contains(t, body, "class balance", "the database checks are still there")
}

func TestHealthWithoutADaemon(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	srv, err := web.New(testConfig(), browser.db, slog.New(slog.DiscardHandler), web.Options{})
	require.NoError(t, err)

	other := &harness{t: t, db: browser.db, srv: srv, handler: srv, cookies: browser.cookies}
	other.cookies = map[string]*http.Cookie{}
	other.enroll(other.token())

	body := other.do(http.MethodGet, "/health", nil).Body.String()
	require.NotContains(t, body, "Daemon")
	require.Contains(t, body, "class balance")
}

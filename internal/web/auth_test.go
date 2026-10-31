package web_test

import (
	"bytes"
	"html"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/store"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	testVersion  = "1.2.3+noweb"
	origin       = "http://localhost:8484"
	registerPath = "/auth/register"
	loginPath    = "/auth/login"
)

type harness struct {
	t       *testing.T
	db      *store.Store
	srv     *web.Server
	actions *fakeActions
	status  *live.Status
	logs    *live.Ring
	handler http.Handler
	cookies map[string]*http.Cookie
}

func testConfig() *config.Config {
	return &config.Config{Web: config.Web{Enabled: true, Listen: "127.0.0.1:8484", Origin: origin}}
}

func newHarness(t *testing.T) *harness {
	t.Helper()

	return newHarnessWith(t, testConfig())
}

func newHarnessWith(t *testing.T, cfg *config.Config, tweaks ...func(*web.Options)) *harness {
	t.Helper()

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = db.Close() })

	actions := &fakeActions{}
	status := live.NewStatus(time.Now())
	logs := live.NewRing(50)

	options := web.Options{Run: actions.run, Status: status, Logs: logs, Version: testVersion}
	for _, tweak := range tweaks {
		tweak(&options)
	}

	srv, err := web.New(cfg, db, slog.New(slog.DiscardHandler), options)
	require.NoError(t, err)

	return &harness{
		t:       t,
		db:      db,
		srv:     srv,
		actions: actions,
		status:  status,
		logs:    logs,
		handler: srv,
		cookies: map[string]*http.Cookie{},
	}
}

// fork is another browser: same server, no cookies.
func (h *harness) fork() *harness {
	return &harness{
		t:       h.t,
		db:      h.db,
		srv:     h.srv,
		actions: h.actions,
		status:  h.status,
		logs:    h.logs,
		handler: h.handler,
		cookies: map[string]*http.Cookie{},
	}
}

func (h *harness) do(
	method, path string,
	body []byte,
	headers ...string,
) *httptest.ResponseRecorder {
	h.t.Helper()

	req := httptest.NewRequestWithContext(h.t.Context(), method, path, bytes.NewReader(body))
	if method == http.MethodPost {
		req.Header.Set("Origin", origin)
	}

	for i := 0; i+1 < len(headers); i += 2 {
		if headers[i+1] == "" {
			req.Header.Del(headers[i])
		} else {
			req.Header.Set(headers[i], headers[i+1])
		}
	}

	for _, c := range h.cookies {
		req.AddCookie(c)
	}

	rec := httptest.NewRecorder()
	h.handler.ServeHTTP(rec, req)

	for _, c := range rec.Result().Cookies() {
		if c.MaxAge < 0 || c.Value == "" {
			delete(h.cookies, c.Name)
		} else {
			h.cookies[c.Name] = c
		}
	}

	return rec
}

func (h *harness) token() string {
	h.t.Helper()

	token, err := web.NewToken()
	require.NoError(h.t, err)
	require.NoError(
		h.t,
		h.db.AddEnrollment(h.t.Context(), web.HashToken(token), time.Now().Add(time.Minute)),
	)

	return token
}

// enroll runs the registration ceremony and returns the new passkey.
func (h *harness) enroll(token string) *authenticator {
	h.t.Helper()

	key := newAuthenticator(h.t, origin)

	begin := h.do(http.MethodPost, registerPath+"/begin?token="+token, nil)
	require.Equal(h.t, http.StatusOK, begin.Code, begin.Body.String())

	finish := h.do(
		http.MethodPost,
		registerPath+"/finish?name=laptop",
		key.register(begin.Body.Bytes()),
	)
	require.Equal(h.t, http.StatusOK, finish.Code, finish.Body.String())

	return key
}

func (h *harness) login(key *authenticator) *httptest.ResponseRecorder {
	h.t.Helper()

	begin := h.do(http.MethodPost, loginPath+"/begin", nil)
	require.Equal(h.t, http.StatusOK, begin.Code, begin.Body.String())

	return h.do(http.MethodPost, loginPath+"/finish", key.login(begin.Body.Bytes()))
}

func TestHomeNeedsLogin(t *testing.T) {
	t.Parallel()

	rec := newHarness(t).do(http.MethodGet, "/", nil)
	require.Equal(t, http.StatusSeeOther, rec.Code)
	require.Equal(t, loginPath, rec.Header().Get("Location"))
}

func TestPagesArePublic(t *testing.T) {
	t.Parallel()

	for _, path := range []string{loginPath, "/auth/enroll?token=x", "/static/webauthn.js"} {
		rec := newHarness(t).do(http.MethodGet, path, nil)
		require.Equal(t, http.StatusOK, rec.Code, path)
	}
}

func TestSecurityHeaders(t *testing.T) {
	t.Parallel()

	rec := newHarness(t).do(http.MethodGet, loginPath, nil)
	require.Contains(t, rec.Header().Get("Content-Security-Policy"), "default-src 'self'")
	require.Contains(t, rec.Header().Get("Content-Security-Policy"), "frame-ancestors 'none'")
	require.Equal(t, "nosniff", rec.Header().Get("X-Content-Type-Options"))
	require.Equal(t, "no-store", rec.Header().Get("Cache-Control"))
}

func TestEnrollThenLogin(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := browser.enroll(browser.token())

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Len(t, keys, 1)
	require.Equal(t, "laptop", keys[0].Name)

	rec := browser.do(http.MethodGet, "/", nil)
	require.Equal(t, http.StatusOK, rec.Code, "enrolling signs you in")

	other := browser.fork()
	require.Equal(t, http.StatusSeeOther, other.do(http.MethodGet, "/", nil).Code)

	rec = other.login(key)
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	require.Equal(t, http.StatusOK, other.do(http.MethodGet, "/", nil).Code)

	keys, err = browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.False(t, keys[0].LastUsed.IsZero())
}

func TestSessionCookieAttributes(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.enroll(browser.token())

	cookie := browser.cookies["tamis_session"]
	require.NotNil(t, cookie)
	require.True(t, cookie.HttpOnly)
	require.Equal(t, http.SameSiteStrictMode, cookie.SameSite)
	require.Equal(t, "/", cookie.Path)
	require.Greater(t, len(cookie.Value), 40)
}

func TestEnrollmentNeedsTokenOrSession(t *testing.T) {
	t.Parallel()

	tests := map[string]string{
		"no token":      registerPath + "/begin",
		"unknown token": registerPath + "/begin?token=nope",
	}

	for name, path := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := newHarness(t).do(http.MethodPost, path, nil)
			require.Equal(t, http.StatusUnauthorized, rec.Code)
		})
	}
}

func TestEnrollmentTokenWorksOnce(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	token := browser.token()
	browser.enroll(token)

	rec := browser.fork().do(http.MethodPost, registerPath+"/begin?token="+token, nil)
	require.Equal(t, http.StatusUnauthorized, rec.Code)
}

func TestSignedInUserAddsPasskey(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.enroll(browser.token())

	second := newAuthenticator(t, origin)

	begin := browser.do(http.MethodPost, registerPath+"/begin", nil)
	require.Equal(t, http.StatusOK, begin.Code, begin.Body.String())

	finish := browser.do(
		http.MethodPost,
		registerPath+"/finish?name=phone",
		second.register(begin.Body.Bytes()),
	)
	require.Equal(t, http.StatusOK, finish.Code, finish.Body.String())

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Len(t, keys, 2)
}

func TestLoginRejectsUnknownPasskey(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.enroll(browser.token())

	stranger := newAuthenticator(t, origin)
	stranger.userHandle = []byte("tamis")

	other := browser.fork()
	rec := other.login(stranger)
	require.Equal(t, http.StatusUnauthorized, rec.Code)
	require.Equal(t, http.StatusSeeOther, other.do(http.MethodGet, "/", nil).Code)
}

func TestLoginRejectsWrongOrigin(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.enroll(browser.token())

	key := newAuthenticator(t, "https://evil.example")
	rec := browser.fork().login(key)
	require.Equal(t, http.StatusUnauthorized, rec.Code)
}

func TestCeremonyWorksOnce(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := browser.enroll(browser.token())

	other := browser.fork()

	begin := other.do(http.MethodPost, loginPath+"/begin", nil)
	response := key.login(begin.Body.Bytes())

	require.Equal(t, http.StatusOK, other.do(http.MethodPost, loginPath+"/finish", response).Code)

	replay := browser.fork()
	replay.cookies = map[string]*http.Cookie{"tamis_ceremony": begin.Result().Cookies()[0]}
	require.Equal(
		t,
		http.StatusUnauthorized,
		replay.do(http.MethodPost, loginPath+"/finish", response).Code,
	)
}

func TestForeignOriginIsRefused(t *testing.T) {
	t.Parallel()

	tests := map[string][]string{
		"other origin": {"Origin", "https://evil.example"},
		"no origin":    {"Origin", ""},
	}

	for name, headers := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			browser := newHarness(t)
			browser.enroll(browser.token())

			for _, path := range []string{
				"/auth/logout", loginPath + "/begin", registerPath + "/begin",
				undoPath, "/decisions/1/learn", whitelistPath, removePath, confirmPath,
			} {
				rec := browser.do(http.MethodPost, path, nil, headers...)
				require.Equal(t, http.StatusForbidden, rec.Code, path)
			}

			require.Equal(
				t,
				http.StatusOK,
				browser.do(http.MethodGet, "/", nil).Code,
				"still signed in",
			)
		})
	}
}

func TestLogout(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.enroll(browser.token())

	stale := browser.cookies["tamis_session"]

	rec := browser.do(http.MethodPost, "/auth/logout", nil)
	require.Equal(t, http.StatusSeeOther, rec.Code)
	require.Equal(t, http.StatusSeeOther, browser.do(http.MethodGet, "/", nil).Code)

	replay := browser.fork()
	replay.cookies["tamis_session"] = stale
	require.Equal(
		t,
		http.StatusSeeOther,
		replay.do(http.MethodGet, "/", nil).Code,
		"the session is gone server-side",
	)
}

func TestPasskeyNameIsBounded(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := newAuthenticator(t, origin)

	begin := browser.do(http.MethodPost, registerPath+"/begin?token="+browser.token(), nil)
	long := string(bytes.Repeat([]byte("x"), 500))
	finish := browser.do(
		http.MethodPost,
		registerPath+"/finish?name="+long,
		key.register(begin.Body.Bytes()),
	)
	require.Equal(t, http.StatusOK, finish.Code)

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.LessOrEqual(t, len(keys[0].Name), 64)
}

func TestLoginCeremonyCannotRegister(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	attacker := newAuthenticator(t, origin)

	begin := browser.do(http.MethodPost, loginPath+"/begin", nil)
	require.Equal(t, http.StatusOK, begin.Code)

	finish := browser.do(
		http.MethodPost,
		registerPath+"/finish?name=evil",
		attacker.register(begin.Body.Bytes()),
	)
	require.Equal(t, http.StatusUnauthorized, finish.Code)

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Empty(t, keys)
	require.Equal(t, http.StatusSeeOther, browser.do(http.MethodGet, "/", nil).Code)
}

func TestSamePasskeyCannotBeEnrolledTwice(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := browser.enroll(browser.token())

	begin := browser.do(http.MethodPost, registerPath+"/begin", nil)
	require.Equal(t, http.StatusOK, begin.Code)

	finish := browser.do(
		http.MethodPost,
		registerPath+"/finish?name=copy",
		key.register(begin.Body.Bytes()),
	)
	require.Equal(t, http.StatusConflict, finish.Code)

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Len(t, keys, 1)
}

func TestLoginRejectsAnotherUsersHandle(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := browser.enroll(browser.token())
	key.userHandle = []byte("someone else")

	require.Equal(t, http.StatusUnauthorized, browser.fork().login(key).Code)
}

func TestPasskeyNameDefaults(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	key := newAuthenticator(t, origin)

	begin := browser.do(http.MethodPost, registerPath+"/begin?token="+browser.token(), nil)
	finish := browser.do(
		http.MethodPost,
		registerPath+"/finish?name=%20%20",
		key.register(begin.Body.Bytes()),
	)
	require.Equal(t, http.StatusOK, finish.Code)

	keys, err := browser.db.Passkeys(t.Context())
	require.NoError(t, err)
	require.Equal(t, "passkey", keys[0].Name)
}

func TestRegistrationFailsCleanlyWhenTheDatabaseDies(t *testing.T) {
	t.Parallel()

	t.Run("while finishing", func(t *testing.T) {
		t.Parallel()

		browser := newHarness(t)
		key := newAuthenticator(t, origin)

		begin := browser.do(http.MethodPost, registerPath+"/begin?token="+browser.token(), nil)
		require.NoError(t, browser.db.Close())

		finish := browser.do(
			http.MethodPost,
			registerPath+"/finish",
			key.register(begin.Body.Bytes()),
		)
		require.Equal(t, http.StatusInternalServerError, finish.Code)
	})

	t.Run("while beginning, signed in", func(t *testing.T) {
		t.Parallel()

		browser := newHarness(t)
		browser.enroll(browser.token())
		require.NoError(t, browser.db.Close())

		begin := browser.do(http.MethodPost, registerPath+"/begin", nil)
		require.Equal(t, http.StatusInternalServerError, begin.Code)
	})
}

func TestPagesShowTheVersion(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	for _, path := range []string{loginPath, "/", healthPath, whitelistPath, "/logs"} {
		body := html.UnescapeString(browser.do(http.MethodGet, path, nil).Body.String())
		require.Contains(t, body, "tamis 1.2.3+noweb", path)
	}

	fragment := browser.do(http.MethodGet, "/", nil, htmxHeader, "true").Body.String()
	require.NotContains(t, fragment, "1.2.3", "a swapped fragment is not a page")
}

func TestNoVersionIsShownWhenThereIsNone(t *testing.T) {
	t.Parallel()

	srv, err := web.New(
		testConfig(),
		newHarness(t).db,
		slog.New(slog.DiscardHandler),
		web.Options{},
	)
	require.NoError(t, err)

	rec := httptest.NewRecorder()
	srv.ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, loginPath, nil))
	require.NotContains(t, rec.Body.String(), "<footer")
}

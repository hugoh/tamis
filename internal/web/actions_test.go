package web_test

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"sync"
	"testing"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	formType   = "application/x-www-form-urlencoded"
	addressKey = "address"
	htmxHeader = "HX-Request"
)

var errGone = errors.New("message not found where tamis left it")

// fakeActions stands in for the engine, which needs a live IMAP connection.
type fakeActions struct {
	mu    sync.Mutex
	calls []string
	err   error
}

func (f *fakeActions) Undo(_ context.Context, decisionID int64) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()

	f.calls = append(f.calls, fmt.Sprintf("undo:%d", decisionID))

	return "moved back to INBOX", f.err
}

func (f *fakeActions) Learn(
	_ context.Context,
	decisionID int64,
	label *classify.Label,
) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()

	name := ""
	if label != nil {
		name = label.String()
	}

	f.calls = append(f.calls, fmt.Sprintf("learn:%d:%s", decisionID, name))

	return "learnt", f.err
}

func (f *fakeActions) run(
	ctx context.Context,
	body func(context.Context, web.Actions) error,
) error {
	return body(ctx, f)
}

func (f *fakeActions) called() []string {
	f.mu.Lock()
	defer f.mu.Unlock()

	return append([]string(nil), f.calls...)
}

func (h *harness) form(path string, values url.Values) *httptest.ResponseRecorder {
	h.t.Helper()

	return h.do(
		http.MethodPost,
		path,
		[]byte(values.Encode()),
		"Content-Type",
		formType,
		htmxHeader,
		"true",
	)
}

func TestDecisionButtons(t *testing.T) {
	t.Parallel()

	body := signedIn(t).do(http.MethodGet, "/", nil).Body.String()

	require.Contains(t, body, `hx-post="/decisions/1/undo"`, "applied: undo")
	require.NotContains(t, body, `/decisions/2/undo`, "shadow-mode decisions have nothing to undo")
	require.Contains(t, body, `hx-post="/decisions/2/learn?label=spam"`)
	require.Contains(t, body, `hx-post="/decisions/2/learn?label=good"`)
	require.Contains(
		t,
		body,
		`hx-post="/decisions/3/learn?label=good"`,
		"uncertain can be taught too",
	)
	require.NotContains(t, body, `/decisions/1/learn`, "applied decisions are undone, not learnt")
}

func TestUndo(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	rec := browser.form(undoPath, nil)
	require.Equal(t, http.StatusOK, rec.Code)
	require.Equal(t, []string{"undo:1"}, browser.actions.called())
	require.Contains(t, rec.Body.String(), "moved back to INBOX")
	require.Contains(t, rec.Body.String(), spamSubject, "the answer is the refreshed row")
	require.NotContains(t, rec.Body.String(), "<html")
}

func TestLearn(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		query string
		want  string
	}{
		"as spam":  {"?label=spam", "learn:2:spam"},
		"as good":  {"?label=good", "learn:2:good"},
		"opposite": {"", "learn:2:"},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			browser := signedIn(t)

			rec := browser.form("/decisions/2/learn"+tt.query, nil)
			require.Equal(t, http.StatusOK, rec.Code)
			require.Equal(t, []string{tt.want}, browser.actions.called())
			require.Contains(t, rec.Body.String(), "learnt")
		})
	}
}

func TestActionFailureIsShownInTheRow(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.actions.err = errGone

	rec := browser.form(undoPath, nil)
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "message not found where tamis left it")
	require.Contains(t, rec.Body.String(), "text-error")
	require.Contains(t, rec.Body.String(), spamSubject)
}

func TestBadActionRequests(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		path string
		code int
	}{
		"unknown decision": {"/decisions/99/undo", http.StatusNotFound},
		"not a number":     {"/decisions/abc/undo", http.StatusBadRequest},
		"bad label":        {"/decisions/2/learn?label=maybe", http.StatusBadRequest},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			browser := signedIn(t)

			require.Equal(t, tt.code, browser.form(tt.path, nil).Code)
			require.Empty(t, browser.actions.called())
		})
	}
}

func TestActionsNeedLogin(t *testing.T) {
	t.Parallel()

	browser := signedIn(t).fork()

	for _, path := range []string{undoPath, "/decisions/2/learn", whitelistPath, removePath} {
		rec := browser.form(path, url.Values{addressKey: {"x@example.com"}})
		require.Equal(t, http.StatusUnauthorized, rec.Code, path)
	}

	require.Empty(t, browser.actions.called())

	addresses, err := browser.db.Addresses(t.Context())
	require.NoError(t, err)
	require.Len(t, addresses, 1, "nothing was added or removed")
}

func TestWhitelistAdd(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	rec := browser.form(whitelistPath, url.Values{addressKey: {"Alice <New@Example.com>"}})
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "new@example.com")
	require.Contains(t, rec.Body.String(), "friend@example.com")
	require.NotContains(t, rec.Body.String(), "<html")

	found, err := browser.db.HasAddress(t.Context(), "new@example.com")
	require.NoError(t, err)
	require.True(t, found)

	addresses, err := browser.db.Addresses(t.Context())
	require.NoError(t, err)

	for _, address := range addresses {
		if address.Address == "new@example.com" {
			require.Equal(t, "manual", address.Source)
		}
	}
}

func TestWhitelistAddRejectsGarbage(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	rec := browser.form(whitelistPath, url.Values{addressKey: {"nope"}})
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), `invalid address &#34;nope&#34;`)
	require.Contains(t, rec.Body.String(), "friend@example.com", "the list is still shown")

	addresses, err := browser.db.Addresses(t.Context())
	require.NoError(t, err)
	require.Len(t, addresses, 1)
}

func TestWhitelistRemove(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	page := browser.do(http.MethodGet, "/whitelist", nil).Body.String()
	require.Contains(t, page, `hx-post="/whitelist/remove"`)
	require.Contains(t, page, `name="address" value="friend@example.com"`)

	rec := browser.form(removePath, url.Values{addressKey: {"friend@example.com"}})
	require.Equal(t, http.StatusOK, rec.Code)
	require.NotContains(t, rec.Body.String(), "friend@example.com")
	require.Contains(t, rec.Body.String(), "No addresses whitelisted")

	found, err := browser.db.HasAddress(t.Context(), "friend@example.com")
	require.NoError(t, err)
	require.False(t, found)
}

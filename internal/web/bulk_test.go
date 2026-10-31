package web_test

import (
	"net/http"
	"net/url"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const (
	notAnAddress = "nope"
	scopeAll     = "all"
	scopeKey     = "scope"
	personSearch = "person"
	confirmPath  = "/whitelist/remove/confirm"
	cancelPath   = "/whitelist/remove/cancel"
	friend       = "friend@example.com"
	person0      = "person000@example.org"
)

func (h *harness) remaining() []string {
	h.t.Helper()

	list, err := h.db.Addresses(h.t.Context())
	require.NoError(h.t, err)

	out := make([]string, len(list))
	for i, entry := range list {
		out[i] = entry.Address
	}

	return out
}

func TestWhitelistRowsCanBeSelected(t *testing.T) {
	t.Parallel()

	body := signedIn(t).do(http.MethodGet, whitelistPath, nil).Body.String()

	require.Contains(
		t,
		body,
		`type="checkbox" class="checkbox checkbox-sm" name="address" value="friend@example.com" form="bulk"`,
	)
	require.Contains(t, body, `id="select-all"`)
	require.Contains(t, body, `<form id="bulk"`)
	require.Contains(t, body, `hx-post="/whitelist/remove/confirm"`)
	require.Contains(t, body, "Remove selected…")
	require.Contains(t, body, "Remove all 1…")
	require.Contains(t, body, `<div id="confirm"></div>`)
	require.Contains(t, body, `src="/static/whitelist.js"`)
	require.NotContains(
		t,
		body,
		`hx-post="/whitelist/remove"`,
		"nothing is removed without a confirmation",
	)
}

func TestWhitelistRemoveAllButtonFollowsTheSearch(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	body := browser.do(http.MethodGet, whitelistPath+"?q=person", nil).Body.String()
	require.Contains(t, body, "Remove 3 matching…")

	none := browser.do(http.MethodGet, whitelistPath+"?q=zzz", nil).Body.String()
	require.NotContains(t, none, "matching…", "nothing to remove all of")
}

func TestWhitelistConfirmListsWhatWillGo(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(1)

	rec := browser.form(
		confirmPath,
		url.Values{addressKey: {"Friend@Example.com", person0, friend}},
	)
	require.Equal(t, http.StatusOK, rec.Code)

	body := rec.Body.String()
	require.NotContains(t, body, "<html")
	require.Contains(t, body, "modal-open")
	require.Contains(t, body, "Remove 2 addresses?", "a repeat counts once")
	require.Equal(t, 1, strings.Count(body, "<li>friend@example.com</li>"))
	require.Contains(t, body, "<li>"+person0+"</li>")
	require.Contains(t, body, `name="address" value="friend@example.com"`)
	require.Contains(t, body, `hx-post="/whitelist/remove"`)
	require.Contains(t, body, `hx-get="/whitelist/remove/cancel"`)
	require.Equal(t, []string{friend, person0}, browser.remaining(), "asking removes nothing")
}

func TestWhitelistConfirmOne(t *testing.T) {
	t.Parallel()

	body := signedIn(t).form(confirmPath, url.Values{addressKey: {friend}}).Body.String()
	require.Contains(t, body, "Remove 1 address?")
}

func TestWhitelistConfirmNeedsASensibleSelection(t *testing.T) {
	t.Parallel()

	tests := map[string]url.Values{
		"nothing selected": {},
		"not an address":   {addressKey: {friend, notAnAddress}},
		"nothing matches":  {scopeKey: {scopeAll}, "q": {"zzz"}},
	}

	for name, values := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := signedIn(t).form(confirmPath, values)
			require.Equal(t, http.StatusOK, rec.Code)
			require.Contains(t, rec.Body.String(), `id="confirm"`)
			require.Contains(t, rec.Body.String(), "alert-warning")
			require.NotContains(t, rec.Body.String(), "modal-open", "there is nothing to confirm")
			require.NotContains(t, rec.Body.String(), `hx-post="/whitelist/remove"`)
		})
	}
}

func TestWhitelistConfirmAllMatching(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	body := browser.form(
		confirmPath,
		url.Values{scopeKey: {scopeAll}, "q": {personSearch}},
	).Body.String()
	require.Contains(t, body, "Remove the 3 addresses matching “person”?")
	require.Contains(t, body, "<li>"+person0+"</li>")
	require.NotContains(t, body, "<li>"+friend+"</li>")
	require.Contains(t, body, `name="scope" value="all"`)
	require.Contains(t, body, `name="q" value="person"`)
	require.Contains(t, body, `name="count" value="3"`)

	everything := browser.form(confirmPath, url.Values{scopeKey: {scopeAll}}).Body.String()
	require.Contains(t, everything, "Remove all 4 addresses?")
}

func TestWhitelistConfirmShowsAtMostSomeAddresses(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.addAddresses(250)
	browser.enroll(browser.token())

	body := browser.form(confirmPath, url.Values{scopeKey: {scopeAll}}).Body.String()
	require.Contains(t, body, "Remove all 250 addresses?")
	require.Equal(t, 200, strings.Count(body, "<li>"))
	require.Contains(t, body, "and 50 more")
}

func TestWhitelistCancel(t *testing.T) {
	t.Parallel()

	rec := signedIn(t).do(http.MethodGet, cancelPath, nil, htmxHeader, "true")
	require.Equal(t, http.StatusOK, rec.Code)
	require.Equal(t, `<div id="confirm"></div>`, strings.TrimSpace(rec.Body.String()))
}

func TestWhitelistRemoveSeveral(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	rec := browser.form(
		"/whitelist/remove",
		url.Values{addressKey: {friend, person0, "PERSON001@example.org"}},
	)
	require.Equal(t, http.StatusOK, rec.Code)

	body := rec.Body.String()
	require.Contains(t, body, "Removed 3 addresses")
	require.Contains(t, body, `<div id="confirm" hx-swap-oob="true"></div>`, "the dialog goes away")
	require.NotContains(t, body, "<td>"+friend)
	require.Contains(t, body, "person002@example.org")
	require.Equal(t, []string{"person002@example.org"}, browser.remaining())
}

func TestWhitelistRemoveOneSaysSo(t *testing.T) {
	t.Parallel()

	body := signedIn(t).form("/whitelist/remove", url.Values{addressKey: {friend}}).Body.String()
	require.Contains(t, body, "Removed 1 address")
	require.NotContains(t, body, "Removed 1 addresses")
}

func TestWhitelistRemoveAllMatching(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	rec := browser.form(
		"/whitelist/remove",
		url.Values{scopeKey: {scopeAll}, "q": {personSearch}, "count": {"3"}},
	)
	require.Contains(t, rec.Body.String(), "Removed 3 addresses")
	require.Equal(t, []string{friend}, browser.remaining())
}

func TestWhitelistRemoveAllMatchingRefusesWhenTheListChanged(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	rec := browser.form(
		"/whitelist/remove",
		url.Values{scopeKey: {scopeAll}, "q": {personSearch}, "count": {"2"}},
	)
	require.Contains(t, rec.Body.String(), "changed since you reviewed it")
	require.Contains(t, rec.Body.String(), `<div id="confirm" hx-swap-oob="true"></div>`)
	require.Len(t, browser.remaining(), 4, "nothing was removed")
}

func TestWhitelistRemoveChecksEverythingFirst(t *testing.T) {
	t.Parallel()

	tests := map[string]url.Values{
		"nothing selected": {},
		"one is garbage":   {addressKey: {friend, notAnAddress}},
	}

	for name, values := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			browser := signedIn(t)

			rec := browser.form("/whitelist/remove", values)
			require.Equal(t, http.StatusOK, rec.Code)
			require.Contains(t, rec.Body.String(), "alert-error")
			require.Equal(t, []string{friend}, browser.remaining(), "no partial removal")
		})
	}
}

func TestWhitelistBulkRemovalNeedsLogin(t *testing.T) {
	t.Parallel()

	browser := signedIn(t).fork()

	for _, path := range []string{confirmPath, "/whitelist/remove"} {
		rec := browser.form(path, url.Values{addressKey: {friend}})
		require.Equal(t, http.StatusUnauthorized, rec.Code, path)
	}

	require.Equal(t, http.StatusSeeOther, browser.do(http.MethodGet, cancelPath, nil).Code)
	require.Equal(t, []string{friend}, browser.remaining())
}

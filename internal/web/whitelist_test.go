package web_test

import (
	"fmt"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func (h *harness) addAddresses(count int) {
	h.t.Helper()

	for i := range count {
		require.NoError(
			h.t,
			h.db.AddAddress(h.t.Context(), fmt.Sprintf("person%03d@example.org", i), "sent"),
		)
	}
}

func TestWhitelistSearch(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	body := browser.do(http.MethodGet, whitelistPath+"?q=PERSON001", nil).Body.String()
	require.Contains(t, body, "person001@example.org")
	require.NotContains(t, body, "person002@example.org")
	require.NotContains(t, body, "friend@example.com")
	require.Contains(t, body, "1 address matches")
	require.Contains(t, body, `name="q" value="PERSON001"`)

	none := browser.do(http.MethodGet, whitelistPath+"?q=zzz", nil).Body.String()
	require.Contains(t, none, "No addresses match.")
}

func TestWhitelistSort(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	order := func(query string) bool {
		body := browser.do(http.MethodGet, whitelistPath+query, nil).Body.String()

		return strings.Index(body, "person000@") < strings.Index(body, "person002@")
	}

	require.True(t, order(""), "alphabetical by default")
	require.True(t, order("?sort=address&dir=asc"))
	require.False(t, order("?sort=address&dir=desc"))

	body := browser.do(http.MethodGet, whitelistPath+"?sort=address&dir=desc", nil).Body.String()
	require.Contains(t, body, "Address ▼")
	require.Regexp(
		t,
		`hx-get="[^"]*sort=source[^"]*dir=asc|hx-get="[^"]*dir=asc[^"]*sort=source`,
		body,
	)
}

func TestWhitelistLoadMoreRows(t *testing.T) {
	t.Parallel()

	browser := newHarness(t)
	browser.addAddresses(230)
	browser.enroll(browser.token())

	first := browser.do(http.MethodGet, whitelistPath, nil).Body.String()
	require.Equal(t, 100, strings.Count(first, "<td>person"))
	require.Contains(t, first, "230 addresses")

	more := regexp.MustCompile(`<tr hx-get="([^"]*)" hx-trigger="intersect once"`).
		FindStringSubmatch(first)
	require.NotNil(t, more)
	require.Contains(t, more[1], "offset=100")

	last := browser.do(
		http.MethodGet,
		whitelistPath+"?offset=200",
		nil,
		htmxHeader,
		"true",
	).Body.String()
	require.Equal(t, 30, strings.Count(last, "<td>person"))
	require.NotContains(t, last, "<table", "only rows")
	require.NotContains(t, last, "intersect once")
}

func TestWhitelistAddResetsTheForm(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)

	added := browser.form(whitelistPath, url.Values{addressKey: {"new@example.org"}}).Body.String()
	require.Contains(t, added, `id="add"`)
	require.Contains(
		t,
		added,
		`hx-swap-oob="true"`,
		"a fresh form replaces the one that was typed in",
	)

	rejected := browser.form(whitelistPath, url.Values{addressKey: {"nope"}}).Body.String()
	require.NotContains(t, rejected, "hx-swap-oob", "a mistake keeps what was typed")
	require.Contains(t, rejected, "alert-error")
}

func TestWhitelistEditsKeepTheSearchAndSort(t *testing.T) {
	t.Parallel()

	browser := signedIn(t)
	browser.addAddresses(3)

	body := browser.form("/whitelist/remove", url.Values{
		addressKey: {
			"person001@example.org",
		},
		"q":    {"person"},
		"sort": {"address"},
		"dir":  {"desc"},
	}).Body.String()

	require.NotContains(t, body, "person001@example.org")
	require.NotContains(t, body, "friend@example.com", "the search still applies")
	require.Contains(t, body, "2 addresses match")
	require.Less(
		t,
		strings.Index(body, "person002@"),
		strings.Index(body, "person000@"),
		"and the sort",
	)
}

package web_test

import (
	"net/http"
	"regexp"
	"testing"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const (
	originalSubject   = "Original message"
	correctionSubject = "Correction of it"
)

// corrected is a harness whose journal holds a decision (#1) and the
// correction made to it later (#2).
func corrected(t *testing.T) *harness {
	t.Helper()

	browser := newHarness(t)
	browser.record(store.Decision{
		MessageID: "<m1@example>", From: "a@example.com", Subject: originalSubject,
		Verdict: engine.VerdictSpam, Reason: bayes, Action: moveToJunk, Applied: true,
	})
	browser.record(store.Decision{
		MessageID: "<m1@example>",
		From:      "a@example.com",
		Subject:   correctionSubject,
		Verdict:   engine.VerdictGood,
		Reason:    engine.ReasonCorrection,
		Action:    "train:good",
		Applied:   true,
	})
	browser.enroll(browser.token())

	return browser
}

func TestCorrectedByLinksToTheEntry(t *testing.T) {
	t.Parallel()

	body := corrected(t).do(http.MethodGet, "/", nil).Body.String()

	link := regexp.MustCompile(`corrected by <a [^>]*href="/\?id=2"[^>]*>#2</a>`)
	require.Regexp(t, link, body)
	require.NotContains(t, body, "corrected by 2", "the number is the link, not plain text")
}

func TestUndoneIsNotALink(t *testing.T) {
	t.Parallel()

	browser := corrected(t)
	require.NoError(t, browser.db.MarkUndone(t.Context(), 1))

	body := browser.do(http.MethodGet, "/", nil).Body.String()
	require.Contains(t, body, "(undone)")
	require.NotRegexp(t, `corrected by`, body)
}

func TestOneEntryCanBeShown(t *testing.T) {
	t.Parallel()

	browser := corrected(t)

	body := browser.do(http.MethodGet, "/?id=2", nil).Body.String()
	require.Contains(t, body, correctionSubject)
	require.NotContains(t, body, originalSubject)
	require.Contains(t, body, "1 decision match")
	require.Contains(t, body, "Showing decision #2 only")
	require.Regexp(t, `<a [^>]*href="/"[^>]*>Show all</a>`, body)

	fragment := browser.do(http.MethodGet, "/?id=2", nil, htmxHeader, "true").Body.String()
	require.Contains(t, fragment, "Showing decision #2 only", "a swapped result says so too")
	require.NotContains(t, fragment, "<html")
}

func TestAnEntryThatIsNotThereShowsNothing(t *testing.T) {
	t.Parallel()

	body := corrected(t).do(http.MethodGet, "/?id=99", nil).Body.String()
	require.Contains(t, body, "Showing decision #99 only")
	require.Contains(t, body, "No decisions match")
}

func TestABadIDIsIgnored(t *testing.T) {
	t.Parallel()

	browser := corrected(t)

	for _, id := range []string{"abc", "0", "-3", "99999999999999999999", ""} {
		body := browser.do(http.MethodGet, "/?id="+id, nil).Body.String()
		require.Contains(t, body, originalSubject, "id=%q", id)
		require.Contains(t, body, correctionSubject, "id=%q", id)
		require.NotContains(t, body, "Showing decision", "id=%q", id)
	}
}

func TestSortingAnEntryKeepsIt(t *testing.T) {
	t.Parallel()

	body := corrected(t).do(http.MethodGet, "/?id=2", nil).Body.String()
	require.Regexp(t, `hx-get="[^"]*id=2[^"]*sort=score|hx-get="[^"]*sort=score[^"]*id=2`, body)
}

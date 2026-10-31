package web_test

import (
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

const (
	rulesPath    = "/rules"
	rulesTestURL = "/rules/test"
	siftScript   = "require [\"fileinto\"];\nif header :contains \"list-id\" \"golang\" { fileinto \"Lists/Go\"; }\n"
	listMail     = "From: g@lists.example\r\nList-Id: <golang-nuts.example>\r\n\r\nhi"
	plainMail    = "From: a@b.example\r\n\r\nhi"
)

func rulesBrowser(t *testing.T, script string) *harness {
	t.Helper()

	cfg := testConfig()

	if script != "" {
		cfg.Rules.Script = filepath.Join(t.TempDir(), "rules.sieve")
		require.NoError(t, os.WriteFile(cfg.Rules.Script, []byte(script), 0o600))
	}

	browser := newHarnessWith(t, cfg)
	browser.enroll(browser.token())

	return browser
}

func TestRulesPageNeedsLogin(t *testing.T) {
	t.Parallel()

	rec := newHarness(t).do(http.MethodGet, rulesPath, nil)
	require.Equal(t, http.StatusSeeOther, rec.Code)
}

func TestRulesPageWithoutScript(t *testing.T) {
	t.Parallel()

	rec := rulesBrowser(t, "").do(http.MethodGet, rulesPath, nil)
	require.Equal(t, http.StatusOK, rec.Code)
	require.Contains(t, rec.Body.String(), "No rules script configured")
	require.Contains(t, rec.Body.String(), `href="/rules"`)
}

func TestRulesPageShowsScript(t *testing.T) {
	t.Parallel()

	body := rulesBrowser(t, siftScript).do(http.MethodGet, rulesPath, nil).Body.String()
	require.Contains(t, body, "rules.sieve")
	require.Contains(t, body, "Valid")
	require.Contains(t, body, `fileinto &#34;Lists/Go&#34;`, "the source is escaped")
}

func TestRulesPageReportsAnInvalidScript(t *testing.T) {
	t.Parallel()

	body := rulesBrowser(t, `fileinto "X";`).do(http.MethodGet, rulesPath, nil).Body.String()
	require.Contains(t, body, "Invalid")
	require.Contains(t, body, "parse rules")
}

func TestRulesTryAMessage(t *testing.T) {
	t.Parallel()

	browser := rulesBrowser(t, siftScript)

	tests := map[string]struct{ message, want string }{
		"match":    {listMail, "Lists/Go"},
		"no match": {plainMail, "Stays in the inbox"},
		"empty":    {"", "Paste a message"},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			rec := browser.form(rulesTestURL, url.Values{"message": {tt.message}})
			require.Equal(t, http.StatusOK, rec.Code)
			require.Contains(t, rec.Body.String(), tt.want)
		})
	}
}

func TestRulesTryWithoutScript(t *testing.T) {
	t.Parallel()

	rec := rulesBrowser(t, "").form(rulesTestURL, url.Values{"message": {plainMail}})
	require.Contains(t, rec.Body.String(), "No rules script configured")
}

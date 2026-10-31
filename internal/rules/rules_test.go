package rules_test

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/rules"
	"github.com/stretchr/testify/require"
)

const script = `require ["fileinto", "body"];
if header :contains "list-id" "golang-nuts" { fileinto "Lists/Go"; stop; }
if address :is "from" "billing@shop.example" { fileinto "Receipts"; }
if body :text :contains "invoice" { fileinto "Invoices"; }
`

func message(headers, body string) []byte {
	return []byte(headers + "\r\n\r\n" + body)
}

func folder(t *testing.T, raw []byte) string {
	t.Helper()

	parsed, err := rules.Parse(strings.NewReader(script))
	require.NoError(t, err)

	got, err := parsed.Folder(context.Background(), raw, "someone@example.org")
	require.NoError(t, err)

	return got
}

func TestFolder(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		raw  []byte
		want string
	}{
		{
			"header match",
			message("From: a@b.c\r\nList-Id: <golang-nuts.googlegroups.com>", "hi"),
			"Lists/Go",
		},
		{
			"address match",
			message("From: Shop <billing@shop.example>", "hi"),
			"Receipts",
		},
		{
			"body match",
			message("From: a@b.c", "Your invoice is attached"),
			"Invoices",
		},
		{"no match", message("From: a@b.c", "hello"), ""},
		{"truncated headers", []byte("From: a@b.c\r\nList-Id: golang-nuts"), "Lists/Go"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			require.Equal(t, tt.want, folder(t, tt.raw))
		})
	}
}

func TestFolderIgnoresRedirect(t *testing.T) {
	t.Parallel()

	parsed, err := rules.Parse(strings.NewReader(`redirect "elsewhere@example.net";`))
	require.NoError(t, err)

	got, err := parsed.Folder(context.Background(), message("From: a@b.c", "hi"), "a@b.c")
	require.NoError(t, err)
	require.Empty(t, got)
}

func TestParseRejectsBadScript(t *testing.T) {
	t.Parallel()

	_, err := rules.Parse(strings.NewReader(`fileinto "X";`))
	require.Error(t, err, "fileinto without require")
}

func TestLoad(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "rules.sieve")
	require.NoError(t, os.WriteFile(path, []byte(script), 0o600))

	_, err := rules.Load(path)
	require.NoError(t, err)

	_, err = rules.Load(filepath.Join(t.TempDir(), "missing.sieve"))
	require.Error(t, err)
}

func TestSenderOf(t *testing.T) {
	t.Parallel()

	tests := []struct{ name, raw, want string }{
		{"named", "From: Gopher <G@Lists.Example>\r\n\r\nhi", "g@lists.example"},
		{"bare", "From: a@b.example\r\n\r\nhi", "a@b.example"},
		{"missing", "Subject: x\r\n\r\nhi", ""},
		{"garbage", "not a message", ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			require.Equal(t, tt.want, rules.SenderOf([]byte(tt.raw)))
		})
	}
}

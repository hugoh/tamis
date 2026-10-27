package classify_test

import (
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/stretchr/testify/require"
)

func message(headers, body string) []byte {
	return []byte(strings.ReplaceAll(headers, "\n", "\r\n") + "\r\n" + body)
}

func TestTokenize(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		raw     []byte
		want    []string
		notWant []string
	}{
		{
			name: "plain text",
			raw: message(`From: Alice Example <alice@example.com>
To: bob@example.org
Subject: Cheap Watches today
Content-Type: text/plain; charset=utf-8
`, "Buy cheap watches! Visit https://deals.shop.example.net/offer?id=1 today.\r\n"),
			want: []string{
				"subject:cheap", "subject:watches", "subject:today",
				"from:alice@example.com", "from-domain:example.com",
				"buy", "cheap", "watches",
				"url:deals.shop.example.net", "url:example.net",
			},
			notWant: []string{"https", "offer"},
		},
		{
			name: "html part",
			raw: message(`From: shop@promo.example
Subject: Offer
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary=b1
`, "--b1\r\n"+
				"Content-Type: text/html; charset=utf-8\r\n\r\n"+
				`<html><body><p>Exclusive <b>discount</b></p>`+
				`<a href="http://track.evil.example/click">Unsubscribe</a></body></html>`+"\r\n"+
				"--b1--\r\n"),
			want:    []string{"exclusive", "discount", "unsubscribe", "url:track.evil.example"},
			notWant: []string{"html", "body", "href"},
		},
		{
			name: "long word",
			raw:  message("Subject: x\nContent-Type: text/plain\n", strings.Repeat("a", 40)+"\r\n"),
			want: []string{"skip:a 40"},
		},
		{
			name:    "short words dropped",
			raw:     message("Subject: x\nContent-Type: text/plain\n", "an ox is ok\r\n"),
			notWant: []string{"an", "ox", "is", "ok"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			tokens := classify.Tokenize(tt.raw)

			for _, want := range tt.want {
				require.Contains(t, tokens, want)
			}

			for _, notWant := range tt.notWant {
				require.NotContains(t, tokens, notWant)
			}
		})
	}
}

func TestTokenizeDeduplicates(t *testing.T) {
	t.Parallel()

	tokens := classify.Tokenize(
		message("Subject: hi\nContent-Type: text/plain\n", "spam, eggs, spam\r\n"),
	)

	count := 0

	for _, token := range tokens {
		if token == "spam" {
			count++
		}
	}

	require.Equal(t, 1, count)
}

func TestTokenizeGarbage(t *testing.T) {
	t.Parallel()

	require.NotPanics(t, func() { classify.Tokenize([]byte("not a mail message at all")) })
}

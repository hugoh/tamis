package classify_test

import (
	"fmt"
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/stretchr/testify/require"
)

const (
	rcptMine  = "rcpt:mine"
	rcptOther = "rcpt:other"
)

func TestTokenizeRecipients(t *testing.T) {
	t.Parallel()

	tokens := classify.Tokenize(message(`From: a@x.example
To: Bob <BOB@Example.org>, carol@example.org
Cc: bob@example.org, dave@other.example
Subject: hi
Content-Type: text/plain
`, "body text\r\n"))

	for _, want := range []string{
		"to:bob@example.org", "to-domain:example.org", "to:carol@example.org",
		"to:dave@other.example", "to-domain:other.example",
	} {
		require.Contains(t, tokens, want)
	}

	count := 0

	for _, token := range tokens {
		if token == "to:bob@example.org" {
			count++
		}
	}

	require.Equal(t, 1, count, "an address in To and Cc counts once")
}

func TestTokenizeCapsRecipients(t *testing.T) {
	t.Parallel()

	to := make([]string, 25)
	for i := range to {
		to[i] = fmt.Sprintf("u%d@example.org", i)
	}

	headers := "To: " + strings.Join(to, ", ") + "\nSubject: hi\nContent-Type: text/plain\n"
	tokens := classify.Tokenize(message(headers, "x\r\n"))

	count := 0

	for _, token := range tokens {
		if strings.HasPrefix(token, "to:") {
			count++
		}
	}

	require.Equal(t, 10, count, "a mail to a big group must not flood the model")
}

func TestTokenizeWithoutRecipients(t *testing.T) {
	t.Parallel()

	for _, token := range classify.Tokenize(message("Subject: hi\nContent-Type: text/plain\n", "x\r\n")) {
		require.False(t, strings.HasPrefix(token, "to:"), token)
	}
}

func status(tokens []string) string {
	for _, token := range tokens {
		if strings.HasPrefix(token, "rcpt:") {
			return token
		}
	}

	return ""
}

func TestWithRecipientStatus(t *testing.T) {
	t.Parallel()

	own := []string{"Me@Example.com"}
	// to builds a token list the way Tokenize would: one to: token per address.
	tokensTo := func(addresses ...string) []string {
		tokens := make([]string, len(addresses))
		for i, address := range addresses {
			tokens[i] = "to:" + address
		}

		return tokens
	}

	model := newMemoryModel()

	goodTo := func(address string, times int) {
		for range times {
			model.Train(tokensTo(address), classify.Good)
		}
	}

	goodTo("alias@example.com", 3)
	goodTo("twice@example.com", 2)

	for range 5 {
		model.Train(tokensTo("junk@example.com"), classify.Spam)
	}

	tests := []struct {
		name      string
		addresses []string
		want      string
	}{
		{"no recipients", nil, "rcpt:none"},
		{"your own address, whatever the case", []string{"me@example.com"}, rcptMine},
		{"seen in three good messages", []string{"alias@example.com"}, rcptMine},
		{"seen in only two", []string{"twice@example.com"}, rcptOther},
		{"spam doesn't teach what to expect", []string{"junk@example.com"}, rcptOther},
		{"never seen", []string{"stranger@example.com"}, rcptOther},
		{
			"one expected address among strangers",
			[]string{"stranger@example.com", "alias@example.com"},
			rcptMine,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			tokens := classify.WithRecipientStatus(tokensTo(tt.addresses...), model, own)
			require.Equal(t, tt.want, status(tokens))
			require.Contains(t, classify.RecipientStatusTokens(), status(tokens))
		})
	}
}

func TestWithRecipientStatusKeepsTheTokens(t *testing.T) {
	t.Parallel()

	tokens := []string{"hello", "to:a@example.com"}
	got := classify.WithRecipientStatus(tokens, newMemoryModel(), nil)

	require.Equal(t, tokens, got[:2])
	require.Len(t, got, 3)
	require.Len(t, tokens, 2, "the caller's slice is left alone")
}

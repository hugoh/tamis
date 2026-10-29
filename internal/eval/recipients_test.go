package eval_test

import (
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/eval"
	"github.com/stretchr/testify/require"
)

func to(address string) []byte {
	return mail("s@x.example", address, "Hi", "body", time.Now())
}

func status(tokens []string) string {
	for _, token := range tokens {
		if len(token) > 5 && token[:5] == "rcpt:" {
			return token
		}
	}

	return ""
}

func TestRecipientStatus(t *testing.T) {
	t.Parallel()

	f := eval.Recipients(eval.RecipientOptions{Status: true, Own: []string{"me@example.com"}, MinSeen: 3, Window: 100}).
		New()

	require.Equal(
		t,
		"rcpt:mine",
		status(f.Tokens(to("ME@example.com"))),
		"your own address is always active",
	)
	require.Equal(t, "rcpt:other", status(f.Tokens(to("alias@example.com"))))
	require.Equal(
		t,
		"rcpt:none",
		status(f.Tokens(mail("s@x.example", "", "Hi", "body", time.Now()))),
	)

	for range 2 {
		f.Learn(to("alias@example.com"), false)
	}

	require.Equal(
		t,
		"rcpt:other",
		status(f.Tokens(to("alias@example.com"))),
		"two sightings aren't enough",
	)

	f.Learn(to("alias@example.com"), false)
	require.Equal(t, "rcpt:mine", status(f.Tokens(to("alias@example.com"))))

	for range 5 {
		f.Learn(to("junk@example.com"), true)
	}

	require.Equal(
		t,
		"rcpt:other",
		status(f.Tokens(to("junk@example.com"))),
		"spam doesn't teach what to expect",
	)
}

func TestRecipientWindowForgetsOldMail(t *testing.T) {
	t.Parallel()

	f := eval.Recipients(eval.RecipientOptions{Status: true, MinSeen: 2, Window: 4}).New()

	f.Learn(to("old@example.com"), false)
	f.Learn(to("old@example.com"), false)
	require.Equal(t, "rcpt:mine", status(f.Tokens(to("old@example.com"))))

	for range 4 {
		f.Learn(to("new@example.com"), false)
	}

	require.Equal(
		t,
		"rcpt:other",
		status(f.Tokens(to("old@example.com"))),
		"retired aliases drop out",
	)
}

func TestRecipientPerAddressTokens(t *testing.T) {
	t.Parallel()

	f := eval.Recipients(eval.RecipientOptions{PerAddress: true, MaxAddresses: 2}).New()

	raw := mail(
		"s@x.example",
		"a@one.example, b@two.example, c@three.example",
		"Hi",
		"body",
		time.Now(),
	)
	tokens := f.Tokens(raw)

	require.Contains(t, tokens, "to:a@one.example")
	require.Contains(t, tokens, "to-domain:one.example")
	require.Contains(t, tokens, "to:b@two.example")
	require.NotContains(
		t,
		tokens,
		"to:c@three.example",
		"capped, so a group mail doesn't flood the model",
	)
	require.Empty(t, status(tokens), "no status token unless asked")
	require.Contains(t, tokens, "body", "the usual tokens are still there")
}

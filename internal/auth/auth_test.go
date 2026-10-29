package auth_test

import (
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/stretchr/testify/require"
)

func message(headers ...string) []byte {
	return []byte(strings.Join(headers, "\r\n") + "\r\n\r\nbody\r\n")
}

func TestVerified(t *testing.T) {
	t.Parallel()

	const from = "From: Friend <friend@example.com>"

	tests := []struct {
		name     string
		raw      []byte
		serverID string
		want     bool
	}{
		{
			name: "dmarc pass for the From domain",
			raw: message(
				"Authentication-Results: mx.icloud.com; dmarc=pass (p=none) header.from=example.com",
				from,
			),
			want: true,
		},
		{
			name: "dkim pass aligned with the From domain",
			raw: message(
				"Authentication-Results: mx.icloud.com; dkim=pass header.d=example.com header.s=s1",
				from,
			),
			want: true,
		},
		{
			name: "dkim pass from a parent domain",
			raw: message(
				"Authentication-Results: mx.icloud.com; dkim=pass header.d=example.com",
				"From: news@mail.example.com",
			),
			want: true,
		},
		{
			name: "dkim pass for an unrelated domain",
			raw: message(
				"Authentication-Results: mx.icloud.com; dkim=pass header.d=mailer.example.net",
				from,
			),
			want: false,
		},
		{
			name: "dmarc fail",
			raw: message(
				"Authentication-Results: mx.icloud.com; dmarc=fail header.from=example.com",
				from,
			),
			want: false,
		},
		{
			name: "results split across lines",
			raw: message(
				"Authentication-Results: mx.icloud.com;",
				"\tspf=pass smtp.mailfrom=example.com;",
				"\tdkim=pass header.d=example.com",
				from,
			),
			want: true,
		},
		{
			name: "forged header below the provider's",
			raw: message(
				"Authentication-Results: mx.icloud.com; dkim=none; dmarc=none",
				"Authentication-Results: mx.icloud.com; dmarc=pass header.from=example.com",
				from,
			),
			want: false,
		},
		{
			name: "authserv-id matches the configured one",
			raw: message(
				"Authentication-Results: MX.iCloud.com 1; dmarc=pass header.from=example.com",
				from,
			),
			serverID: "mx.icloud.com",
			want:     true,
		},
		{
			name: "header written by someone other than the provider",
			raw: message(
				"Authentication-Results: evil.example.net; dmarc=pass header.from=example.com",
				from,
			),
			serverID: "mx.icloud.com",
			want:     false,
		},
		{
			name: "pass smuggled inside a quoted reason",
			raw: message(
				`Authentication-Results: mx.icloud.com; dmarc=fail reason="x; dmarc=pass header.from=example.com ; y"`,
				from,
			),
			want: false,
		},
		{
			name: "pass smuggled after a nested comment",
			raw: message(
				"Authentication-Results: mx.icloud.com; dmarc=fail (a (b) ; dmarc=pass header.from=example.com )",
				from,
			),
			want: false,
		},
		{
			name: "pass smuggled after an escaped quote",
			raw: message(
				`Authentication-Results: mx.icloud.com; dmarc=fail reason="a\"; dmarc=pass header.from=example.com ; y"`,
				from,
			),
			want: false,
		},
		{
			name: "pass smuggled after an escaped parenthesis in a comment",
			raw: message(
				`Authentication-Results: mx.icloud.com; dmarc=fail (a \) ; dmarc=pass header.from=example.com )`,
				from,
			),
			want: false,
		},
		{
			name: "no results",
			raw:  message(from),
			want: false,
		},
		{
			name: "no sender",
			raw: message(
				"Authentication-Results: mx.icloud.com; dmarc=pass header.from=example.com",
			),
			want: false,
		},
		{
			name: "garbage",
			raw:  []byte("not a message"),
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			require.Equal(t, tt.want, auth.Verified(tt.raw, tt.serverID))
		})
	}
}

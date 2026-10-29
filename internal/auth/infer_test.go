package auth_test

import (
	"fmt"
	"testing"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/stretchr/testify/require"
)

func topped(id string) []byte {
	return message(
		fmt.Sprintf("Authentication-Results: %s; dmarc=none", id),
		"From: Friend <friend@example.com>",
	)
}

func bare() []byte { return message("From: Friend <friend@example.com>") }

func TestTallyInfer(t *testing.T) {
	t.Parallel()

	repeat := func(raw []byte, n int) [][]byte {
		out := make([][]byte, n)
		for i := range out {
			out[i] = raw
		}

		return out
	}

	tests := []struct {
		name    string
		samples [][]byte
		want    string
	}{
		{"provider on every message", repeat(topped("bimi.icloud.com"), 30), "icloud.com"},
		{
			"provider on nine in ten",
			append(repeat(topped("bimi.icloud.com"), 27), repeat(bare(), 3)...),
			"icloud.com",
		},
		{
			"provider on fewer than nine in ten",
			append(repeat(topped("bimi.icloud.com"), 26), repeat(bare(), 4)...),
			"",
		},
		{
			"two providers",
			append(repeat(topped("mx.google.com"), 15), repeat(topped("bimi.icloud.com"), 15)...),
			"",
		},
		{"too few messages", repeat(topped("bimi.icloud.com"), 5), ""},
		{"an id under a two-level suffix", repeat(topped("mx.example.co.uk"), 30), "example.co.uk"},
		{"an id that is not a domain", repeat(topped("ATOM_Relay"), 30), ""},
		{"a bare public suffix", repeat(topped("co.uk"), 30), ""},
		{"no headers at all", repeat(bare(), 30), ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			var tally auth.Tally

			for _, raw := range tt.samples {
				tally.Add(raw)
			}

			require.Equal(t, len(tt.samples), tally.Total())

			got, ok := tally.Infer()
			require.Equal(t, tt.want != "", ok)
			require.Equal(t, tt.want, got)
		})
	}
}

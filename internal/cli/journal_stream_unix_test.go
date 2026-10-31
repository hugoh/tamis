//go:build unix

package cli

import (
	"bytes"
	"io"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestUnderJournalChecksTheStreamAgainstStderr(t *testing.T) {
	t.Parallel()

	file, err := os.Create(filepath.Join(t.TempDir(), "stderr"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = file.Close() })

	own, ok := fileStream(file)
	require.True(t, ok)

	tests := []struct {
		name   string
		stream string
		out    io.Writer
		want   bool
	}{
		{"not under systemd", "", file, false},
		{"stderr is the journal's stream", own, file, true},
		{"stderr was redirected elsewhere", "1:2", file, false},
		{"not a file, so trust the variable", "1:2", &bytes.Buffer{}, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			require.Equal(t, tt.want, underJournal(tt.stream, tt.out))
		})
	}
}

package cli_test

import (
	"io"
	"os"
	"testing"

	"github.com/hugoh/tamis/internal/cli"
	"github.com/stretchr/testify/require"
)

func TestTerminalPromptWithoutTerminal(t *testing.T) {
	t.Parallel()

	reader, writer, err := os.Pipe()
	require.NoError(t, err)

	t.Cleanup(func() {
		_ = reader.Close()
		_ = writer.Close()
	})

	_, err = cli.TerminalPrompt(reader, io.Discard)("Password: ")
	require.ErrorIs(t, err, cli.ErrNotTerminal)
}

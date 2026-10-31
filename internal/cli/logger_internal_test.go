package cli

import (
	"bytes"
	"log/slog"
	"testing"

	"github.com/spf13/cobra"
	"github.com/stretchr/testify/require"
)

func TestLoggerFollowsALevelThatChangesWhileRunning(t *testing.T) {
	t.Parallel()

	for name, journal := range map[string]string{"terminal": "", "journal": "8:1"} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			var stderr bytes.Buffer

			cmd := &cobra.Command{}
			cmd.SetErr(&stderr)

			app := &app{env: Env{Getenv: func(key string) string {
				if key == "JOURNAL_STREAM" {
					return journal
				}

				return ""
			}}}

			var level slog.LevelVar

			logger := app.loggerAt(cmd, &level)

			logger.Debug("before")
			require.NotContains(t, stderr.String(), "before")

			level.Set(slog.LevelDebug)
			logger.Debug("during")
			require.Contains(t, stderr.String(), "during")

			level.Set(slog.LevelInfo)
			logger.Debug("after")
			require.NotContains(t, stderr.String(), "after")
		})
	}
}

func TestBaseLevelFollowsVerbose(t *testing.T) {
	t.Parallel()

	require.Equal(t, slog.LevelInfo, (&app{}).baseLevel())
	require.Equal(t, slog.LevelDebug, (&app{verbose: true}).baseLevel())
}

package config_test

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

const account = "[account]\nhost = \"h\"\nuser = \"u\"\n"

func rulesConfig(t *testing.T, script string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "rules.sieve")
	require.NoError(t, os.WriteFile(path, []byte(script), 0o600))

	return write(t, account+fmt.Sprintf("[rules]\nscript = %q\n", path))
}

func TestRulesOffByDefault(t *testing.T) {
	t.Parallel()

	cfg, err := config.Load(noEnv, write(t, account))
	require.NoError(t, err)
	require.Empty(t, cfg.Rules.Script)
	require.False(t, cfg.Rules.CreateFolders, "rules never invent folders unless told to")
}

func TestRulesCreateFolders(t *testing.T) {
	t.Parallel()

	cfg, err := config.Load(noEnv, write(t, account+"[rules]\ncreate_folders = true\n"))
	require.NoError(t, err)
	require.True(t, cfg.Rules.CreateFolders)
}

func TestRulesScriptIsChecked(t *testing.T) {
	t.Parallel()

	cfg, err := config.Load(noEnv, rulesConfig(t, `require "fileinto"; fileinto "X";`))
	require.NoError(t, err)
	require.NotEmpty(t, cfg.Rules.Script)
}

func TestRulesInvalid(t *testing.T) {
	t.Parallel()

	tests := []struct{ name, body string }{
		{"missing file", fmt.Sprintf("[rules]\nscript = %q\n", "/nonexistent/rules.sieve")},
		{"syntax error", ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			path := write(t, account+tt.body)
			if tt.body == "" {
				path = rulesConfig(t, `fileinto "X";`)
			}

			_, err := config.Load(noEnv, path)
			require.ErrorContains(t, err, "rules.script")
		})
	}
}

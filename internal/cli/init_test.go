package cli_test

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

const (
	initCmd  = "init"
	initHost = "imap.example.com"
)

func TestInit(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), "tamis", "config.toml")
	getenv := func(string) string { return "" }

	got := execute(t.Context(), t, invocation{
		args: []string{
			initCmd,
			configFlag,
			path,
			hostFlag,
			initHost,
			userFlag,
			`me"@example.com`,
		},
	})
	require.Zero(t, got.code, "stderr: %s", got.stderr)
	require.Contains(t, got.stdout, path)
	require.Contains(t, got.stdout, "tamis login")

	cfg, err := config.Load(getenv, path)
	require.NoError(t, err)
	require.Equal(t, initHost, cfg.Account.Host)
	require.Equal(t, `me"@example.com`, cfg.Account.User)

	if runtime.GOOS != "windows" {
		info, err := os.Stat(path)
		require.NoError(t, err)
		require.Equal(t, os.FileMode(0o600), info.Mode().Perm())
	}
}

func TestInitRefusals(t *testing.T) {
	t.Parallel()

	existing := filepath.Join(t.TempDir(), "config.toml")
	require.NoError(t, os.WriteFile(existing, []byte("keep me"), 0o600))

	tests := []struct {
		name       string
		args       []string
		wantStderr string
		wantCode   int
	}{
		{
			name:       "existing file",
			args:       []string{initCmd, configFlag, existing, hostFlag, "h", userFlag, "u"},
			wantStderr: "already exists",
			wantCode:   1,
		},
		{
			name:       "without host",
			args:       []string{initCmd, configFlag, existing, userFlag, "u"},
			wantStderr: `"host" not set`,
			wantCode:   2,
		},
		{
			name:       "without user",
			args:       []string{initCmd, configFlag, existing, hostFlag, "h"},
			wantStderr: `"user" not set`,
			wantCode:   2,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got := execute(t.Context(), t, invocation{args: tt.args})
			require.Equal(t, tt.wantCode, got.code)
			require.Contains(t, got.stderr, tt.wantStderr)

			kept, err := os.ReadFile(existing) //nolint:gosec // test temp file
			require.NoError(t, err)
			require.Equal(t, "keep me", string(kept))
		})
	}
}

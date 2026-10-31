package cli

import (
	"log/slog"
	"os"
	"path/filepath"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

func TestReloadSwapsInTheConfigOnlyWhenItLoads(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name     string
		body     string
		wantHost string
	}{
		{"valid file replaces the config", "[account]\nhost = \"new\"\nuser = \"u\"", "new"},
		{"broken file keeps the old config", "[account\n", "old"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			path := filepath.Join(t.TempDir(), "tamis.toml")
			require.NoError(t, os.WriteFile(path, []byte(tt.body), 0o600))

			a := &app{env: Env{Getenv: func(string) string { return "" }}, configPath: path}
			old := &config.Config{Account: config.Account{Host: "old"}}

			got, _ := a.reload(t.Context(), old, nil, slog.New(slog.DiscardHandler))
			require.Equal(t, tt.wantHost, got.Account.Host)
		})
	}
}

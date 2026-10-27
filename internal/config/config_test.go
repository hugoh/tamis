package config_test

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

func write(t *testing.T, body string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "config.toml")
	require.NoError(t, os.WriteFile(path, []byte(body), 0o600))

	return path
}

func TestLoadDefaults(t *testing.T) {
	t.Parallel()

	cfg, err := config.Load(write(t, `
[account]
host = "imap.mail.me.com"
user = "me@icloud.com"
`))
	require.NoError(t, err)

	require.Equal(t, "imap.mail.me.com", cfg.Account.Host)
	require.Equal(t, 993, cfg.Account.Port)
	require.Equal(t, []string{"INBOX"}, cfg.Folders.Watch)
	require.Equal(t, "Junk", cfg.Folders.Junk)
	require.Equal(t, "Sent Messages", cfg.Folders.Sent)
	require.Equal(t, "TrainSpam", cfg.Folders.TrainSpam)
	require.Equal(t, "TrainGood", cfg.Folders.TrainGood)
	require.Equal(t, config.ModeShadow, cfg.Policy.Mode)
	require.InDelta(t, 0.9, cfg.Policy.SpamThreshold, 1e-9)
	require.InDelta(t, 0.2, cfg.Policy.GoodThreshold, 1e-9)
	require.Equal(t, 2*time.Minute, cfg.Policy.PollInterval.Duration)
	require.True(t, cfg.Policy.WhitelistRequiresAuth)
	require.NotEmpty(t, cfg.State)
}

func TestLoadOverrides(t *testing.T) {
	t.Parallel()

	home, err := os.UserHomeDir()
	require.NoError(t, err)

	cfg, err := config.Load(write(t, `
state = "~/tamis-test.db"

[account]
host = "imap.example.com"
port = 1993
user = "me@example.com"

[folders]
watch = ["INBOX", "Lists"]
junk = "Spam"

[policy]
mode = "live"
spam_threshold = 0.95
good_threshold = 0.1
poll_interval = "30s"
whitelist_requires_auth = false
`))
	require.NoError(t, err)

	require.Equal(t, "imap.example.com:1993", cfg.Account.Addr())
	require.Equal(t, []string{"INBOX", "Lists"}, cfg.Folders.Watch)
	require.Equal(t, "Spam", cfg.Folders.Junk)
	require.Equal(t, config.ModeLive, cfg.Policy.Mode)
	require.Equal(t, 30*time.Second, cfg.Policy.PollInterval.Duration)
	require.False(t, cfg.Policy.WhitelistRequiresAuth)
	require.Equal(t, filepath.Join(home, "tamis-test.db"), cfg.State)
}

func TestLoadInvalid(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name, body, wantErr string
	}{
		{"missing user", "[account]\nhost = \"h\"", "account.user"},
		{"missing host", "[account]\nuser = \"u\"", "account.host"},
		{
			"bad mode",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\nmode = \"yolo\"",
			"policy.mode",
		},
		{
			"thresholds inverted",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\nspam_threshold = 0.2\ngood_threshold = 0.5",
			"threshold",
		},
		{
			"no watched folders",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[folders]\nwatch = []",
			"folders.watch",
		},
		{"unknown key", "[account]\nhost = \"h\"\nuser = \"u\"\ntypo = 1", "typo"},
		{"bad toml", "[account", "parse"},
		{
			"bad duration",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\npoll_interval = \"soon\"",
			"poll_interval",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			_, err := config.Load(write(t, tt.body))
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestLoadMissingFile(t *testing.T) {
	t.Parallel()

	_, err := config.Load(filepath.Join(t.TempDir(), "absent.toml"))
	require.ErrorIs(t, err, os.ErrNotExist)
}

func TestPassword(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		account config.Account
		env     map[string]string
		want    string
		wantErr string
	}{
		{
			name:    "command",
			account: config.Account{PasswordCommand: []string{"echo", "s3cret"}},
			want:    "s3cret",
		},
		{
			name: "environment",
			env:  map[string]string{"TAMIS_PASSWORD": "from-env"},
			want: "from-env",
		},
		{
			name:    "command wins over environment",
			account: config.Account{PasswordCommand: []string{"echo", "cmd"}},
			env:     map[string]string{"TAMIS_PASSWORD": "env"},
			want:    "cmd",
		},
		{name: "none", wantErr: "password_command"},
		{
			name:    "failing command",
			account: config.Account{PasswordCommand: []string{"false"}},
			wantErr: "password_command",
		},
		{
			name:    "empty output",
			account: config.Account{PasswordCommand: []string{"true"}},
			wantErr: "empty",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got, err := tt.account.Password(
				t.Context(),
				func(key string) string { return tt.env[key] },
			)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)

				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}

func TestDefaultPath(t *testing.T) {
	t.Parallel()

	path := config.DefaultPath(func(key string) string {
		return map[string]string{"XDG_CONFIG_HOME": "/xdg"}[key]
	})
	require.Equal(t, "/xdg/tamis/config.toml", path)

	path = config.DefaultPath(func(string) string { return "" })
	require.Equal(
		t,
		filepath.Join("tamis", "config.toml"),
		filepath.Join(filepath.Base(filepath.Dir(path)), filepath.Base(path)),
	)
}

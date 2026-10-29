package config_test

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

func noEnv(string) string { return "" }

func write(t *testing.T, body string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "config.toml")
	require.NoError(t, os.WriteFile(path, []byte(body), 0o600))

	return path
}

func TestLoadDefaults(t *testing.T) {
	t.Parallel()

	cfg, err := config.Load(noEnv, write(t, `
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
	require.True(t, cfg.Policy.LearnFromJunk)
	require.True(t, cfg.Policy.LearnProviderJunk)
	require.True(t, cfg.Policy.LearnFromKeywords)
	require.True(t, cfg.Policy.Rescue)
	require.Equal(t, 365*24*time.Hour, cfg.Policy.JournalRetention.Duration)
	require.Zero(t, cfg.Policy.RescueThreshold)
	require.Equal(t, "INBOX", cfg.Folders.RescueTo())
	require.NotEmpty(t, cfg.State)
}

func TestLoadOverrides(t *testing.T) {
	t.Parallel()

	home, err := os.UserHomeDir()
	require.NoError(t, err)

	cfg, err := config.Load(noEnv, write(t, `
state = "~/tamis-test.db"

[account]
host = "imap.example.com"
port = 1993
user = "me@example.com"
auth_server_id = "mx.example.com"

[folders]
watch = ["INBOX", "Lists"]
junk = "Spam"
rescued = "Rescued"

[policy]
mode = "live"
spam_threshold = 0.95
good_threshold = 0.1
poll_interval = "30s"
whitelist_requires_auth = false
learn_from_junk = false
learn_from_keywords = false
rescue = false
rescue_threshold = 0.05
journal_retention = "720h"
`))
	require.NoError(t, err)

	require.Equal(t, "imap.example.com:1993", cfg.Account.Addr())
	require.Equal(t, "mx.example.com", cfg.Account.AuthServerID)
	require.Equal(t, []string{"INBOX", "Lists"}, cfg.Folders.Watch)
	require.Equal(t, "Spam", cfg.Folders.Junk)
	require.Equal(t, config.ModeLive, cfg.Policy.Mode)
	require.Equal(t, 30*time.Second, cfg.Policy.PollInterval.Duration)
	require.False(t, cfg.Policy.WhitelistRequiresAuth)
	require.False(t, cfg.Policy.LearnFromJunk)
	require.False(t, cfg.Policy.LearnFromKeywords)
	require.False(t, cfg.Policy.Rescue)
	require.InDelta(t, 0.05, cfg.Policy.RescueThreshold, 1e-9)
	require.Equal(t, 720*time.Hour, cfg.Policy.JournalRetention.Duration)
	require.Equal(t, "Rescued", cfg.Folders.RescueTo())
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
			"rescue threshold too high",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\nrescue_threshold = 0.3",
			"rescue_threshold",
		},
		{
			"negative rescue threshold",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\nrescue_threshold = -0.1",
			"rescue_threshold",
		},
		{
			"negative journal retention",
			"[account]\nhost = \"h\"\nuser = \"u\"\n[policy]\njournal_retention = \"-1h\"",
			"journal_retention",
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

			_, err := config.Load(noEnv, write(t, tt.body))
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestLoadMissingFile(t *testing.T) {
	t.Parallel()

	_, err := config.Load(noEnv, filepath.Join(t.TempDir(), "absent.toml"))
	require.ErrorIs(t, err, os.ErrNotExist)
}

func TestPasswordCommand(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		command []string
		want    string
		wantErr error
		errText string
	}{
		{name: "prints the password", command: []string{"echo", "s3cret"}, want: "s3cret"},
		{name: "not configured", wantErr: config.ErrNoPasswordCommand},
		{name: "fails", command: []string{"false"}, errText: "password_command"},
		{name: "prints nothing", command: []string{"true"}, errText: "empty"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got, err := config.Account{PasswordCommand: tt.command}.Password(t.Context())

			switch {
			case tt.wantErr != nil:
				require.ErrorIs(t, err, tt.wantErr)
			case tt.errText != "":
				require.ErrorContains(t, err, tt.errText)
			default:
				require.NoError(t, err)
				require.Equal(t, tt.want, got)
			}
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

func TestNoHomeDirectory(t *testing.T) {
	t.Setenv("HOME", "")

	none := func(string) string { return "" }

	require.Empty(t, config.DefaultPath(none))
	require.Empty(t, config.DefaultStatePath(none))

	_, err := config.Load(none, "")
	require.ErrorContains(t, err, "--config")
}

func TestStateFollowsXDG(t *testing.T) {
	t.Parallel()

	home, err := os.UserHomeDir()
	require.NoError(t, err)

	const minimal = "[account]\nhost = \"h\"\nuser = \"u\"\n"

	tests := []struct {
		name string
		env  map[string]string
		want string
	}{
		{
			"XDG_STATE_HOME set",
			map[string]string{"XDG_STATE_HOME": "/xdg/state"},
			"/xdg/state/tamis/tamis.db",
		},
		{"XDG_STATE_HOME unset", nil, filepath.Join(home, ".local", "state", "tamis", "tamis.db")},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			cfg, err := config.Load(
				func(key string) string { return tt.env[key] },
				write(t, minimal),
			)
			require.NoError(t, err)
			require.Equal(t, tt.want, cfg.State)
		})
	}
}

func TestLoadRefusesConfigOthersCanWrite(t *testing.T) {
	t.Parallel()

	if runtime.GOOS == "windows" {
		t.Skip("POSIX permissions")
	}

	path := write(t, "[account]\nhost = \"h\"\nuser = \"u\"\n")

	for _, mode := range []os.FileMode{0o620, 0o602} {
		require.NoError(t, os.Chmod(path, mode))

		_, err := config.Load(noEnv, path)
		require.ErrorContains(t, err, "writable by others")
	}

	// Readable by others is allowed: the file holds no secrets.
	require.NoError(t, os.Chmod(path, 0o644)) //nolint:gosec // see above

	_, err := config.Load(noEnv, path)
	require.NoError(t, err)
}

func TestPasswordCommandTimesOut(t *testing.T) {
	t.Parallel()

	start := time.Now()
	_, err := config.Account{
		PasswordCommand: []string{"sh", "-c", "sleep 10 & wait"},
	}.PasswordWithin(t.Context(), 200*time.Millisecond)

	require.ErrorContains(t, err, "password_command")
	require.Less(t, time.Since(start), 5*time.Second)
}

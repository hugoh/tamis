package engine_test

import (
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/stretchr/testify/require"
)

const (
	icloudBIMI  = "Authentication-Results: bimi.icloud.com; bimi=none"
	icloudDMARC = "Authentication-Results: dmarc.icloud.com; dmarc=pass header.from=example.com"
	authSetting = "auth_server_id"
	icloudID    = "icloud.com"
	hostSetting = "auth_server_id_host"
	icloudHost  = "imap.mail.me.com"
)

func TestBootstrapInfersAuthServerID(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	old := time.Now().AddDate(0, 0, -30)

	for range 25 {
		_, err := e.client.Append(inbox,
			e.message("old@good.example", "Project meeting", "Agenda for the meeting.", icloudBIMI),
			[]string{seenFlag}, old)
		require.NoError(t, err)
	}

	summary, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Good:       []engine.Source{{Folder: inbox, Limit: 100}},
		GoodMinAge: 7 * 24 * time.Hour,
	})
	require.NoError(t, err)
	require.Equal(t, icloudID, summary.AuthServerID)

	value, found, err := e.db.Setting(t.Context(), authSetting)
	require.NoError(t, err)
	require.True(t, found)
	require.Equal(t, icloudID, value)

	host, _, err := e.db.Setting(t.Context(), hostSetting)
	require.NoError(t, err)
	require.Equal(t, e.cfg.Account.Host, host, "remember which account it was learnt for")
}

func TestBootstrapLeavesAuthServerIDAloneWhenUnclear(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	require.NoError(t, e.db.SetSetting(t.Context(), authSetting, icloudID))

	_, err := e.client.Append(inbox, e.hammy("old@good.example"), []string{seenFlag},
		time.Now().AddDate(0, 0, -30))
	require.NoError(t, err)

	summary, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Good:       []engine.Source{{Folder: inbox, Limit: 100}},
		GoodMinAge: 7 * 24 * time.Hour,
	})
	require.NoError(t, err)
	require.Empty(t, summary.AuthServerID)

	value, _, err := e.db.Setting(t.Context(), authSetting)
	require.NoError(t, err)
	require.Equal(t, icloudID, value, "a sample too small to tell must not erase what was learnt")
}

func TestAuthServerIDGoesStaleWhenTheHostChanges(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		learntFor string
		wantSrc   engine.AuthSource
		wantID    string
	}{
		{"same host", icloudHost, engine.AuthLearnt, icloudID},
		{"host differs in case", "IMAP.Mail.Me.com", engine.AuthLearnt, icloudID},
		{"learnt before hosts were recorded", "", engine.AuthLearnt, icloudID},
		{"different host", "imap.gmail.com", engine.AuthStale, ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.cfg.Account.Host = icloudHost

			require.NoError(t, e.db.SetSetting(t.Context(), authSetting, icloudID))

			if tt.learntFor != "" {
				require.NoError(t, e.db.SetSetting(t.Context(), hostSetting, tt.learntFor))
			}

			pin, err := engine.ResolveAuthPin(t.Context(), e.db, e.cfg)
			require.NoError(t, err)
			require.Equal(t, tt.wantSrc, pin.Source)
			require.Equal(t, tt.wantID, pin.ID)

			if tt.wantSrc == engine.AuthStale {
				require.Equal(t, "imap.gmail.com", pin.LearntFor)
				require.Equal(t, icloudHost, pin.Host)
			}
		})
	}
}

func TestStaleAuthServerIDIsNotUsed(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.cfg.Account.Host = "imap.gmail.com"
	require.NoError(t, e.db.SetSetting(t.Context(), authSetting, icloudID))
	require.NoError(t, e.db.SetSetting(t.Context(), hostSetting, icloudHost))
	require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))

	e.run(t)
	e.append(t, inbox, e.message(friend, "Hello", "Plain words.", icloudBIMI, icloudDMARC))
	e.run(t)

	require.Equal(t, engine.ReasonBayes, e.decisions(t)[0].Reason)
}

func TestAuthPinSources(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		config string
		want   engine.AuthSource
	}{
		{"nothing", "", engine.AuthUnset},
		{"configured", icloudID, engine.AuthConfigured},
		{"switched off", engine.NoAuthServerID, engine.AuthOff},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.cfg.Account.AuthServerID = tt.config

			pin, err := engine.ResolveAuthPin(t.Context(), e.db, e.cfg)
			require.NoError(t, err)
			require.Equal(t, tt.want, pin.Source)
		})
	}
}

func TestAuthServerIDResolution(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		config  string
		learnt  string
		wantWL  bool
		comment string
	}{
		{"nothing configured or learnt", "", "", false, "topmost header only: BIMI"},
		{"learnt from the mailbox", "", icloudID, true, ""},
		{"configured", icloudID, "", true, ""},
		{"configuration overrides what was learnt", "example.net", icloudID, false, ""},
		{"none switches pinning off", engine.NoAuthServerID, icloudID, false, ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.cfg.Account.AuthServerID = tt.config

			if tt.learnt != "" {
				require.NoError(t, e.db.SetSetting(t.Context(), authSetting, tt.learnt))
			}

			require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
			e.run(t)
			e.append(t, inbox, e.message(friend, "Hello", "Plain words.", icloudBIMI, icloudDMARC))
			e.run(t)

			reason := e.decisions(t)[0].Reason
			if tt.wantWL {
				require.Equal(t, engine.ReasonWhitelisted, reason)
			} else {
				require.Equal(t, engine.ReasonBayes, reason)
			}
		})
	}
}

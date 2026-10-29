package engine_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/stretchr/testify/require"
)

func TestLearnProviderJunk(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name        string
		mode        config.Mode
		off         bool
		prepare     func(t *testing.T, e *env)
		trained     bool
		rescued     int
		whitelisted bool
	}{
		{
			name: "mail the model is unsure about",
			mode: config.ModeShadow,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(
					t,
					junk,
					e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."),
				)
			},
			trained: true,
		},
		{
			name: "applies in live mode too",
			mode: config.ModeLive,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(
					t,
					junk,
					e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."),
				)
			},
			trained: true,
		},
		{
			name: "mail the model already calls spam",
			mode: config.ModeShadow,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, junk, e.spammy("s@spam.example"))
			},
		},
		{
			name: "learning from provider Junk turned off",
			mode: config.ModeShadow,
			off:  true,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(
					t,
					junk,
					e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."),
				)
			},
		},
		{
			name: "mail that is rescued",
			mode: config.ModeShadow,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.fromFriend(t, dmarcOK)
			},
			rescued:     1,
			whitelisted: true,
		},
		{
			name: "whitelisted sender that failed authentication stays whitelisted",
			mode: config.ModeShadow,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
				e.append(t, junk, e.message(friend, "Quokka", "Xylophone zebra quokka."))
			},
			trained:     true,
			whitelisted: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, tt.mode)
			e.cfg.Policy.LearnProviderJunk = !tt.off
			e.trainModel(t)
			e.run(t)
			tt.prepare(t, e)

			summary := e.run(t)
			require.Equal(t, tt.rescued, summary.Rescued)

			filed := e.messages(t, junk)
			if tt.rescued == 0 {
				require.Len(t, filed, 1)

				_, trained, err := e.db.Trained(t.Context(), filed[0].MessageID)
				require.NoError(t, err)
				require.Equal(t, tt.trained, trained)
				require.Equal(t, btoi(tt.trained), summary.Trained)
			}

			if tt.trained {
				decision := e.decisions(t)[0]
				require.Equal(t, engine.ReasonProviderJunk, decision.Reason)
				require.Equal(t, engine.VerdictSpam, decision.Verdict)
				require.Equal(t, "train:spam", decision.Action)
				require.True(t, decision.Applied)
			}

			if tt.whitelisted {
				has, err := e.db.HasAddress(t.Context(), friend)
				require.NoError(t, err)
				require.True(t, has)
			}
		})
	}
}

func btoi(b bool) int {
	if b {
		return 1
	}

	return 0
}

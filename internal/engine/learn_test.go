package engine_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/stretchr/testify/require"
)

func TestLearnShadowSpamIsLearntAsGood(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("friend@example.org"))
	require.Equal(t, 1, e.run(t).Spam)

	summary, err := e.engine.Learn(t.Context(), e.latestDecisionID(t), nil)
	require.NoError(t, err)
	require.Contains(t, summary, "learnt as good")

	whitelisted, err := e.db.HasAddress(t.Context(), "friend@example.org")
	require.NoError(t, err)
	require.True(t, whitelisted)

	learnt := e.decisions(t)[0]
	require.Equal(t, engine.ReasonCorrection, learnt.Reason)
	require.Equal(t, engine.VerdictGood, learnt.Verdict)
	require.Len(t, e.messages(t, inbox), 1, "the message stays where it is")
}

func TestLearnShadowGoodIsLearntAsSpam(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	require.NoError(t, e.db.AddAddress(t.Context(), "friend@example.org", "test"))
	e.append(t, inbox, e.hammy("friend@example.org"))
	e.run(t)

	summary, err := e.engine.Learn(t.Context(), e.latestDecisionID(t), nil)
	require.NoError(t, err)
	require.Contains(t, summary, "learnt as spam")

	whitelisted, err := e.db.HasAddress(t.Context(), "friend@example.org")
	require.NoError(t, err)
	require.False(t, whitelisted)
}

func TestLearnUncertainNeedsLabel(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."))
	require.Equal(t, 1, e.run(t).Uncertain)
	id := e.latestDecisionID(t)

	_, err := e.engine.Learn(t.Context(), id, nil)
	require.ErrorIs(t, err, engine.ErrNeedsLabel)

	spam := classify.Spam
	summary, err := e.engine.Learn(t.Context(), id, &spam)
	require.NoError(t, err)
	require.Contains(t, summary, "learnt as spam")
}

func TestLearnRefusals(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		prepare func(t *testing.T, e *env) int64
		want    error
	}{
		{
			name: "applied in live mode",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()

				e.cfg.Policy.Mode = config.ModeLive
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				return e.latestDecisionID(t)
			},
			want: engine.ErrAlreadyApplied,
		},
		{
			name: "training",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, trainSpam, e.spammy("s@spam.example"))
				e.run(t)

				return e.latestDecisionID(t)
			},
			want: engine.ErrNotLearnable,
		},
		{
			name: "already corrected",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				id := e.latestDecisionID(t)
				_, err := e.engine.Learn(t.Context(), id, nil)
				require.NoError(t, err)

				return id
			},
			want: engine.ErrAlreadyCorrected,
		},
		{
			name: "message gone",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				moved := e.messages(t, inbox)
				require.NoError(t, e.client.Move(inbox, []uint32{moved[0].UID}, trainSpam))

				return e.latestDecisionID(t)
			},
			want: engine.ErrMessageGone,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.trainModel(t)
			e.run(t)

			_, err := e.engine.Learn(t.Context(), tt.prepare(t, e), nil)
			require.ErrorIs(t, err, tt.want)
		})
	}
}

func TestLearnWithALabelOverridesAnEarlierCorrection(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("s@spam.example"))
	e.run(t)

	id := e.latestDecisionID(t)
	_, err := e.engine.Learn(t.Context(), id, nil)
	require.NoError(t, err)

	spam := classify.Spam
	summary, err := e.engine.Learn(t.Context(), id, &spam)
	require.NoError(t, err)
	require.Contains(t, summary, "learnt as spam")
}

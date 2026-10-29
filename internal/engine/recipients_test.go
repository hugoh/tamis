package engine_test

import (
	"fmt"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/stretchr/testify/require"
)

const alias = "alias@example.com"

// sentTo is a message addressed to the given address.
func (e *env) sentTo(address, from, subject string) []byte {
	e.serial++

	return fmt.Appendf(
		nil,
		"From: %s\r\nTo: %s\r\nSubject: %s\r\nMessage-ID: <r%d@test>\r\nContent-Type: text/plain\r\n\r\n%s\r\n",
		from,
		address,
		subject,
		e.serial,
		"Words about the project and the meeting agenda.",
	)
}

func TestBootstrapTeachesWhichAddressesAreExpected(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	old := time.Now().AddDate(0, 0, -30)

	for i := range 4 {
		_, err := e.client.Append(
			inbox,
			e.sentTo(alias, fmt.Sprintf("friend%d@good.example", i), "Hello"),
			[]string{seenFlag},
			old,
		)
		require.NoError(t, err)
	}

	e.append(t, junk, e.sentTo(alias, "spammer@spam.example", "Hello"))

	_, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Spam:       []engine.Source{{Folder: junk, Limit: 100}},
		Good:       []engine.Source{{Folder: inbox, Limit: 100}},
		GoodMinAge: 7 * 24 * time.Hour,
	})
	require.NoError(t, err)

	model, err := e.db.Model(t.Context(), []string{"rcpt:mine", "rcpt:other", "to:" + alias})
	require.NoError(t, err)

	spamMine, goodMine := model.Counts("rcpt:mine")
	spamOther, goodOther := model.Counts("rcpt:other")
	spamTo, goodTo := model.Counts("to:" + alias)

	require.Equal(
		t,
		1,
		spamMine,
		"good mail is learnt first, so the spam is seen as live mail would be",
	)
	require.Equal(t, 1, goodMine, "the fourth message to an address it had seen three times")
	require.Zero(t, spamOther)
	require.Equal(t, 3, goodOther, "the first three, before the address was known")
	require.Equal(t, 1, spamTo)
	require.Equal(t, 4, goodTo)
}

func TestMailToAnUnexpectedAddressScoresWorse(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.cfg.Account.User = "me@example.com"
	e.trainModel(t)

	// Plenty of good mail to the alias, and spam to a stranger's address.
	for i := range 6 {
		e.append(
			t,
			trainGood,
			e.sentTo(alias, fmt.Sprintf("friend%d@good.example", i), "Project meeting"),
		)
		e.append(
			t,
			trainSpam,
			e.sentTo(
				"stranger@elsewhere.example",
				fmt.Sprintf("s%d@spam.example", i),
				"Project meeting",
			),
		)
	}

	e.run(t)

	e.append(t, inbox, e.sentTo(alias, "new@somewhere.example", "Project meeting"))
	e.append(
		t,
		inbox,
		e.sentTo("stranger@elsewhere.example", "new@elsewhere.example", "Project meeting"),
	)
	e.run(t)

	scores := map[string]float64{}

	for _, decision := range e.decisions(t) {
		scores[decision.From] = decision.Score
	}

	require.Greater(t, scores["new@elsewhere.example"], scores["new@somewhere.example"],
		"the same words, but only one went to an address good mail goes to")
}

func TestBootstrapRefreshRetokenizesWhatWasLearnt(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		refresh   bool
		wantTo    int // good messages that now carry to:<alias>
		wantStale int // good messages still carrying the old token
		refreshed int
	}{
		{"a plain bootstrap leaves what was learnt alone", false, 0, 4, 0},
		{"refresh re-tokenizes it", true, 4, 0, 4},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			old := time.Now().AddDate(0, 0, -30)

			// Messages learnt by an older tokenizer: one token, "legacy".
			for i := range 4 {
				_, err := e.client.Append(
					inbox,
					e.sentTo(alias, fmt.Sprintf("friend%d@good.example", i), "Hello"),
					[]string{seenFlag},
					old,
				)
				require.NoError(t, err)
			}

			for _, message := range e.messages(t, inbox) {
				_, err := e.db.Train(
					t.Context(),
					message.MessageID,
					[]string{"legacy"},
					classify.Good,
				)
				require.NoError(t, err)
			}

			summary, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
				Good:       []engine.Source{{Folder: inbox, Limit: 100}},
				GoodMinAge: 7 * 24 * time.Hour,
				Refresh:    tt.refresh,
			})
			require.NoError(t, err)
			require.Equal(t, tt.refreshed, summary.Refreshed)

			model, err := e.db.Model(t.Context(), []string{"legacy", "to:" + alias})
			require.NoError(t, err)

			_, stale := model.Counts("legacy")
			_, withTo := model.Counts("to:" + alias)
			_, totalGood := model.Totals()

			require.Equal(t, tt.wantStale, stale)
			require.Equal(t, tt.wantTo, withTo)
			require.Equal(t, 4, totalGood, "refreshing never counts a message twice")
		})
	}
}

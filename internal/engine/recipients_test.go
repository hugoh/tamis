package engine_test

import (
	"fmt"
	"testing"
	"time"

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

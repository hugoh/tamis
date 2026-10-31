package engine_test

import (
	"slices"
	"strings"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/rules"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const listsRule = `require ["fileinto"];
if header :contains "list-id" "golang-nuts" { fileinto "Lists/Go"; }
`

const listsFolder = "Lists/Go"

func (e *env) withRules(t *testing.T, script string) {
	t.Helper()

	parsed, err := rules.Parse(strings.NewReader(script))
	require.NoError(t, err)

	e.engine.SetRules(parsed)
}

func (e *env) fromList(from string) []byte {
	return e.message(from, "Project meeting", "Agenda for the project meeting with the team.",
		"List-Id: <golang-nuts.googlegroups.com>")
}

func TestRulesFileGoodMail(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.withRules(t, listsRule)
	e.run(t)

	e.append(t, inbox, e.fromList("g@good.example"))
	e.append(t, inbox, e.hammy("h@good.example"))

	summary := e.run(t)

	require.Equal(t, engine.Summary{Scanned: 2, Good: 2}, summary)

	filed := e.messages(t, listsFolder)
	require.Len(t, filed, 1, "the folder is created on demand")
	require.Equal(t, "g@good.example", filed[0].From)
	require.Contains(t, filed[0].Flags, "$tamissorted")
	require.Len(t, e.messages(t, inbox), 1, "unmatched mail stays")

	decisions := e.decisions(t)
	require.Len(t, decisions, 2)

	i := slices.IndexFunc(
		decisions,
		func(d store.Decision) bool { return d.From == "g@good.example" },
	)
	require.GreaterOrEqual(t, i, 0)
	require.Equal(t, "good", decisions[i].Verdict)
	require.Equal(t, engine.ReasonRule, decisions[i].Reason)
	require.Equal(t, "move:"+listsFolder, decisions[i].Action)
	require.True(t, decisions[i].Applied)
}

func TestRulesNeverFileSpamOrUncertain(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.withRules(t, `require ["fileinto"]; fileinto "Everything";`)
	e.run(t)

	e.append(t, inbox, e.message("s@spam.example", "WINNER claim prize",
		"Congratulations winner, claim your lottery prize now.",
		"List-Id: <golang-nuts.googlegroups.com>"))
	e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."))

	e.run(t)

	require.Len(t, e.messages(t, junk), 1)
	require.Len(t, e.messages(t, inbox), 1, "uncertain stays for the user")
}

func TestRulesInShadowModeOnlyRecord(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.withRules(t, listsRule)
	e.run(t)

	e.append(t, inbox, e.fromList("g@good.example"))
	e.run(t)

	require.Len(t, e.messages(t, inbox), 1)

	decisions := e.decisions(t)
	require.Len(t, decisions, 1)
	require.Equal(t, "move:"+listsFolder, decisions[0].Action)
	require.False(t, decisions[0].Applied)
}

func TestRulesLeaveSortedMailAlone(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.withRules(t, listsRule)
	e.run(t)

	e.append(t, inbox, e.fromList("g@good.example"))
	e.markFrom(t, inbox, "g@good.example", engine.SortedKeyword)

	summary := e.run(t)

	require.Equal(t, engine.Summary{}, summary)
	require.Len(t, e.messages(t, inbox), 1, "dragged back by the user, not refiled")
}

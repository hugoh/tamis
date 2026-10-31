package engine_test

import (
	"bytes"
	"log/slog"
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

func (e *env) fromList() []byte {
	return e.message(
		"g@good.example",
		"Project meeting",
		"Agenda for the project meeting with the team.",
		"List-Id: <golang-nuts.googlegroups.com>",
	)
}

func TestRulesFileGoodMail(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.cfg.Rules.CreateFolders = true
	e.trainModel(t)
	e.withRules(t, listsRule)
	e.run(t)

	e.append(t, inbox, e.fromList())
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
	require.NoError(t, e.client.Ensure(listsFolder))
	e.run(t)

	e.append(t, inbox, e.fromList())
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

	e.append(t, inbox, e.fromList())
	e.markFrom(t, inbox, "g@good.example", engine.SortedKeyword)

	summary := e.run(t)

	require.Equal(t, engine.Summary{}, summary)
	require.Len(t, e.messages(t, inbox), 1, "dragged back by the user, not refiled")
}

func TestRulesNeedAnExistingFolder(t *testing.T) {
	t.Parallel()

	for _, mode := range []config.Mode{config.ModeLive, config.ModeShadow} {
		t.Run(string(mode), func(t *testing.T) {
			t.Parallel()

			e := setup(t, mode)
			e.trainModel(t)
			e.withRules(t, listsRule)
			e.run(t)

			e.append(t, inbox, e.fromList())
			e.run(t)

			require.Len(t, e.messages(t, inbox), 1, "a typo'd rule leaves the mail alone")

			exists, err := e.client.Exists(listsFolder)
			require.NoError(t, err)
			require.False(t, exists, "and doesn't invent the folder")

			decisions := e.decisions(t)
			require.Len(t, decisions, 1)
			require.Equal(t, engine.ReasonBayes, decisions[0].Reason)
		})
	}
}

func TestRulesFileIntoAnExistingFolder(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.withRules(t, listsRule)
	require.NoError(t, e.client.Ensure(listsFolder))
	e.run(t)

	e.append(t, inbox, e.fromList())
	e.run(t)

	require.Len(t, e.messages(t, listsFolder), 1)
	require.Empty(t, e.messages(t, inbox))
}

func (e *env) startupWarnings(t *testing.T, script string) string {
	t.Helper()

	parsed, err := rules.Parse(strings.NewReader(script))
	require.NoError(t, err)

	var logs bytes.Buffer

	logged := engine.New(e.client, e.db, e.cfg, slog.New(slog.NewTextHandler(&logs, nil)))
	logged.SetRules(parsed)
	require.NoError(t, logged.WarnMissingRuleFolders(t.Context()))

	return logs.String()
}

func TestWarnMissingRuleFolders(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	require.NoError(t, e.client.Ensure("code"))
	require.NoError(t, e.client.Ensure("Lists/Go"))

	logs := e.startupWarnings(t, `require ["fileinto"]; fileinto "Lists/Gp"; fileinto "code";`)
	require.Contains(t, logs, "level=WARN")
	require.Contains(t, logs, "target=Lists/Gp")
	require.Contains(t, logs, "nearest=Lists/Go")
	require.NotContains(t, logs, "target=code")
}

func TestNoWarningWhenFoldersExistOrAreCreated(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	require.Contains(t, e.startupWarnings(t, listsRule), listsFolder)

	require.NoError(t, e.client.Ensure(listsFolder))
	require.Empty(t, e.startupWarnings(t, listsRule))

	other := setup(t, config.ModeLive)
	other.cfg.Rules.CreateFolders = true
	require.Empty(t, other.startupWarnings(t, listsRule), "missing folders will be created")
}

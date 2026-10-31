package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/rules"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

const (
	defaultSentLimit  = 2000
	defaultCorpusSize = 500
	defaultGoodMinAge = 7 * 24 * time.Hour
	defaultLogLimit   = 20
	passTimeout       = 10 * time.Minute
	// newMailSettle lets a burst of arrivals land before the next pass.
	newMailSettle    = time.Second
	watchRetry       = time.Minute
	bootstrapTimeout = time.Hour
	tablePadding     = 2
	stateDirMode     = 0o700
)

// app holds what the account-based commands share.
type app struct {
	env        Env
	version    string
	configPath string
	finder     passwordFinder
	ran        bool
	verbose    bool
	color      string
}

// logger logs to stderr, including debug lines under --verbose. Under systemd
// (which sets $JOURNAL_STREAM for a service whose output goes to the journal)
// each line carries its priority, for `journalctl -p`.
func (a *app) logger(cmd *cobra.Command) *slog.Logger {
	return a.loggerAt(cmd, a.baseLevel())
}

// baseLevel is the level the daemon logs at: info, or debug under --verbose.
func (a *app) baseLevel() slog.Level {
	if a.verbose {
		return slog.LevelDebug
	}

	return slog.LevelInfo
}

// loggerAt is logger with a level that may change while it runs.
func (a *app) loggerAt(cmd *cobra.Command, level slog.Leveler) *slog.Logger {
	if a.env.Getenv("JOURNAL_STREAM") != "" {
		return slog.New(newJournalHandler(cmd.ErrOrStderr(), level))
	}

	return slog.New(slog.NewTextHandler(cmd.ErrOrStderr(), &slog.HandlerOptions{Level: level}))
}

// withStore opens the configuration and state database for the duration of body.
func (a *app) withStore(
	cmd *cobra.Command,
	body func(context.Context, *config.Config, *store.Store) error,
) error {
	ctx := cmd.Context()

	cfg, database, err := a.open(ctx)
	if err != nil {
		return err
	}

	defer func() { _ = database.Close() }()

	return body(ctx, cfg, database)
}

// withStats is withStore for commands that report on what the database holds.
func (a *app) withStats(
	cmd *cobra.Command,
	body func(context.Context, *config.Config, *store.Store, store.Stats) error,
) error {
	return a.withStore(
		cmd,
		func(ctx context.Context, cfg *config.Config, database *store.Store) error {
			stats, err := database.Stats(ctx)
			if err != nil {
				return err //nolint:wrapcheck // store errors carry their context
			}

			return body(ctx, cfg, database, stats)
		},
	)
}

// open loads the configuration and opens the state database.
func (a *app) open(ctx context.Context) (*config.Config, *store.Store, error) {
	cfg, err := config.Load(a.env.Getenv, a.configPath)
	if err != nil {
		return nil, nil, err //nolint:wrapcheck // config errors already name the file
	}

	if err := os.MkdirAll(filepath.Dir(cfg.State), stateDirMode); err != nil {
		return nil, nil, fmt.Errorf("create state directory: %w", err)
	}

	database, err := store.Open(ctx, cfg.State)
	if err != nil {
		return nil, nil, fmt.Errorf("open state: %w", err)
	}

	return cfg, database, nil
}

func (a *app) connect(
	ctx context.Context,
	cfg *config.Config,
	logger *slog.Logger,
) (*mail.Client, error) {
	password, err := a.password(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("connect: %w", err)
	}

	client, err := mail.Dial(ctx, a.dialConfig(cfg, password, logger))
	if err != nil {
		return nil, fmt.Errorf("connect: %w", err)
	}

	return client, nil
}

func (a *app) dialConfig(cfg *config.Config, password string, logger *slog.Logger) mail.DialConfig {
	return mail.DialConfig{
		Addr:      cfg.Account.Addr(),
		Username:  cfg.Account.User,
		Password:  password,
		TLSConfig: a.env.TLSConfig,
		Logger:    logger,
	}
}

// waitForNextPass blocks until the watcher hears of new mail or flag changes,
// or poll_interval passes: the poll covers the folders that aren't watched
// and a watcher that has dropped out.
func waitForNextPass(
	ctx context.Context,
	cfg *config.Config,
	watcher *mail.Watcher,
	status *live.Status,
) {
	if watcher.Idling() > 0 {
		status.Wait(live.Idle)
	} else {
		status.Wait(live.Polling)
	}

	select {
	case <-watcher.Changed():
		pause(ctx, newMailSettle)
	case <-time.After(cfg.Policy.PollInterval.Duration):
	case <-ctx.Done():
	}
}

// startWatcher IDLEs on the watched folders. It watches nothing when the
// password can't be found; polling still covers them.
func (a *app) startWatcher(
	ctx context.Context,
	cfg *config.Config,
	logger *slog.Logger,
) *mail.Watcher {
	password, err := a.password(ctx, cfg)
	if err != nil {
		logger.WarnContext(ctx, "not watching for new mail; polling", "err", err)

		return mail.Watch(ctx, mail.DialConfig{}, nil, watchRetry)
	}

	return mail.Watch(ctx, a.dialConfig(cfg, password, logger), cfg.Folders.Watch, watchRetry)
}

func pause(ctx context.Context, d time.Duration) {
	select {
	case <-ctx.Done():
	case <-time.After(d):
	}
}

// password finds the account's password; see passwordFinder.
func (a *app) password(ctx context.Context, cfg *config.Config) (string, error) {
	return a.finder.find(ctx, credentials{
		host:    cfg.Account.Host,
		user:    cfg.Account.User,
		command: cfg.Account.PasswordCommand,
	})
}

// withEngine runs fn against a fresh connection and the state database.
func (a *app) withEngine(
	ctx context.Context,
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	timeout time.Duration,
	body func(context.Context, *engine.Engine) error,
) error {
	client, err := a.connect(ctx, cfg, logger)
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	defer client.CloseWhenDone(ctx)()

	eng := engine.New(client, database, cfg, logger)

	if cfg.Rules.Script != "" {
		loaded, err := rules.Load(cfg.Rules.Script)
		if err != nil {
			return err //nolint:wrapcheck // rules errors already name the problem
		}

		eng.SetRules(loaded)
	}

	return body(ctx, eng)
}

func (a *app) bootstrapCommand() *cobra.Command {
	var (
		opts       engine.BootstrapOptions
		spam, good map[string]int
	)

	cmd := &cobra.Command{
		Use:   "bootstrap",
		Short: "Seed the whitelist and the filter from existing mail",
		Long: "Seed the whitelist from Sent recipients and the filter from Junk (as spam) and\n" +
			"old read inbox mail (as good), then start watching from current mail.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, cfg *config.Config, database *store.Store) error {
					if err := chooseSources(cmd, cfg, &opts, spam, good); err != nil {
						return err
					}

					var summary engine.BootstrapSummary

					logger := a.logger(cmd)

					err := a.withEngine(ctx, cfg, database, logger, bootstrapTimeout,
						func(ctx context.Context, eng *engine.Engine) error {
							var err error

							summary, err = eng.Bootstrap(ctx, opts)

							return err //nolint:wrapcheck // engine errors carry their context
						})
					if err != nil {
						return err
					}

					printBootstrap(cmd.OutOrStdout(), cfg, opts, summary)

					return nil
				},
			)
		},
	}

	addBootstrapFlags(cmd, &opts, &spam, &good)

	return cmd
}

func (a *app) runCommand() *cobra.Command {
	var once bool

	cmd := &cobra.Command{
		Use:   "run",
		Short: "Judge new mail, then keep watching",
		Long: "Train from the training folders and judge new mail in the watched folders,\n" +
			"every policy.poll_interval until interrupted. In shadow mode decisions are only\n" +
			"recorded; see them with 'tamis log'.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, cfg *config.Config, database *store.Store) error {
					return a.watch(ctx, cmd, cfg, database, once)
				},
			)
		},
	}

	cmd.Flags().BoolVar(&once, "once", false, "make a single pass and exit")

	return cmd
}

// watch serves the web interface, if enabled, and runs passes until
// interrupted, or just one when once is set.
func (a *app) watch(
	ctx context.Context,
	cmd *cobra.Command,
	cfg *config.Config,
	database *store.Store,
	once bool,
) error {
	logs := live.NewRing(logRingSize)
	debug := live.NewDebugSwitch(a.baseLevel(), debugSpan)
	logger := slog.New(logs.Handler(a.loggerAt(cmd, debug.Level()).Handler()))
	status := live.NewStatus(time.Now())

	stopWeb, err := a.startWeb(ctx, cfg, database, logger, status, logs, debug)
	if err != nil {
		return err
	}

	defer stopWeb()

	var pruner journalPruner

	// Watching starts after the first good pass: a rejected login should be
	// tried once, not once per folder.
	var watcher *mail.Watcher

	defer func() {
		if watcher != nil {
			watcher.Close()
		}
	}()

	checkRules := cfg.Rules.Script != ""

	for {
		if err := pruner.prune(
			ctx, database, cfg.Policy.JournalRetention.Duration, time.Now(), logger,
		); err != nil {
			logger.ErrorContext(ctx, "journal pruning failed", "err", err)
		}

		status.Wait(live.Running)

		err := a.pass(ctx, cfg, database, logger, status, &checkRules)
		if once || errors.Is(err, mail.ErrLogin) {
			return err
		}

		if ctx.Err() != nil {
			return nil
		}

		watcher = a.rest(ctx, cfg, logger, status, watcher, err)
	}
}

// rest waits for the next pass, starting the watcher after the first good
// pass. A failed pass only sleeps, so a persistent failure isn't retried at
// the pace of mail arriving.
func (a *app) rest(
	ctx context.Context,
	cfg *config.Config,
	logger *slog.Logger,
	status *live.Status,
	watcher *mail.Watcher,
	passErr error,
) *mail.Watcher {
	if passErr != nil {
		logger.ErrorContext(ctx, "pass failed", "err", passErr)
		status.Failed(time.Now(), passErr)
		status.Wait(live.Polling)
		pause(ctx, cfg.Policy.PollInterval.Duration)

		return watcher
	}

	if watcher == nil {
		watcher = a.startWatcher(ctx, cfg, logger)
	}

	waitForNextPass(ctx, cfg, watcher, status)

	return watcher
}

func (a *app) pass(
	ctx context.Context,
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	status *live.Status,
	checkRules *bool,
) error {
	return a.withEngine(
		ctx,
		cfg,
		database,
		logger,
		passTimeout,
		func(ctx context.Context, eng *engine.Engine) error {
			if *checkRules {
				*checkRules = false

				if err := eng.WarnMissingRuleFolders(ctx); err != nil {
					logger.WarnContext(ctx, "could not check the rule folders", "err", err)
				}
			}

			summary, err := eng.RunOnce(ctx)
			if err != nil {
				return err //nolint:wrapcheck // engine errors carry their context
			}

			status.PassDone(time.Now(), summaryText(summary))
			logger.InfoContext(ctx, "pass complete", "mode", cfg.Policy.Mode,
				"scanned", summary.Scanned, "spam", summary.Spam, "uncertain", summary.Uncertain,
				"good", summary.Good, "corrected", summary.Corrected, "trained", summary.Trained,
				"rescued", summary.Rescued)

			return nil
		},
	)
}

func summaryText(summary engine.Summary) string {
	return fmt.Sprintf(
		"scanned %d, spam %d, uncertain %d, good %d, corrected %d, trained %d, rescued %d",
		summary.Scanned,
		summary.Spam,
		summary.Uncertain,
		summary.Good,
		summary.Corrected,
		summary.Trained,
		summary.Rescued,
	)
}

func (a *app) logCommand() *cobra.Command {
	limit := defaultLogLimit

	cmd := &cobra.Command{
		Use:   "log",
		Short: "Show recent decisions",
		Long: "Show recent decisions, newest first. In shadow mode, actions read\n" +
			"\"would ...\": what live mode would have done.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			ctx := cmd.Context()

			_, database, err := a.open(ctx)
			if err != nil {
				return err
			}

			defer func() { _ = database.Close() }()

			decisions, err := database.Decisions(ctx, limit)
			if err != nil {
				return err //nolint:wrapcheck // store errors carry their context
			}

			return a.writeDecisions(cmd, decisions)
		},
	}

	cmd.Flags().IntVarP(&limit, "number", "n", limit, "how many decisions to show")

	return cmd
}

// verdictStyles colours a verdict, so the ones that need a look stand out.
var verdictStyles = map[string]style{ //nolint:gochecknoglobals // constant lookup
	engine.VerdictSpam:      styleRed,
	engine.VerdictUncertain: styleYellow,
	engine.VerdictGood:      styleGreen,
}

func (a *app) writeDecisions(cmd *cobra.Command, decisions []store.Decision) error {
	out := cmd.OutOrStdout()

	if len(decisions) == 0 {
		_, _ = fmt.Fprintln(out, "No decisions yet.")

		return nil
	}

	colors, err := a.paletteFor(out)
	if err != nil {
		return err
	}

	text := func(value string, s style) cell { return cell{{value, s}} }

	rows := [][]cell{{
		text("ID", styleBold), text("TIME", styleBold), text("VERDICT", styleBold),
		text("REASON", styleBold), text("SCORE", styleBold), text("ACTION", styleBold),
		text("FROM", styleBold), text("SUBJECT", styleBold),
	}}

	for _, decision := range decisions {
		when := decision.At.Local() //nolint:gosmopolitan // a local CLI shows local time
		verdict := verdictStyles[decision.Verdict]

		rows = append(rows, []cell{
			text(strconv.FormatInt(decision.ID, 10), ""),
			text(when.Format(time.DateTime), styleDim),
			text(decision.Verdict, verdict),
			text(decision.Reason, styleDim),
			text(fmt.Sprintf("%.2f", decision.Score), verdict),
			actionCell(decision),
			text(decision.From, ""),
			text(decision.Subject, ""),
		})
	}

	return writeTable(out, colors, rows)
}

// actionCell shows what was done; what live mode would do, and nothing, are
// dimmed, and a later undo or correction stands out.
func actionCell(decision store.Decision) cell {
	action := cell{{decision.Action, ""}}

	switch {
	case decision.Action == "none":
		action = cell{{decision.Action, styleDim}}
	case !decision.Applied:
		action = cell{{"would " + decision.Action, styleDim}}
	}

	switch {
	case decision.Undone:
		action = append(action, span{" (undone)", styleMagenta})
	case decision.CorrectedBy > 0:
		action = append(
			action,
			span{fmt.Sprintf(" (corrected by %d)", decision.CorrectedBy), styleMagenta},
		)
	}

	return action
}

func (a *app) undoCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "undo ID...",
		Short: "Reverse decisions tamis applied",
		Long: "Reverse decisions tamis applied, by the IDs 'tamis log' shows. Mail filed\n" +
			"to Junk is moved back, learnt as good and its sender whitelisted; an\n" +
			"uncertain flag is cleared.",
		Args: cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			ids, err := parseIDs(args)
			if err != nil {
				return err
			}

			return a.onDecisions(cmd, ids,
				func(ctx context.Context, eng *engine.Engine, id int64) (string, error) {
					return eng.Undo(ctx, id)
				})
		},
	}
}

func (a *app) learnCommand() *cobra.Command {
	var good, spam bool

	cmd := &cobra.Command{
		Use:   "learn ID...",
		Short: "Teach tamis the right answer for decisions it only recorded",
		Long: "Teach tamis the right answer for decisions it recorded without acting on, by\n" +
			"the IDs 'tamis log' shows (shadow mode). With no flag it learns the opposite\n" +
			"of the verdict. The mail stays where it is. Use 'tamis undo' for decisions\n" +
			"that were applied.",
		Args: cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			ids, err := parseIDs(args)
			if err != nil {
				return err
			}

			var label *classify.Label

			switch {
			case good:
				label = new(classify.Good)
			case spam:
				label = new(classify.Spam)
			}

			return a.onDecisions(cmd, ids,
				func(ctx context.Context, eng *engine.Engine, id int64) (string, error) {
					return eng.Learn(ctx, id, label)
				})
		},
	}
	cmd.Flags().BoolVar(&good, "good", false, "learn as good")
	cmd.Flags().BoolVar(&spam, "spam", false, "learn as spam")
	cmd.MarkFlagsMutuallyExclusive("good", "spam")

	return cmd
}

// onDecisions applies act to each decision in turn, reporting each outcome,
// and fails if any of them did.
func (a *app) onDecisions(
	cmd *cobra.Command,
	ids []int64,
	act func(context.Context, *engine.Engine, int64) (string, error),
) error {
	return a.withStore(
		cmd,
		func(ctx context.Context, cfg *config.Config, database *store.Store) error {
			return a.withEngine(ctx, cfg, database, a.logger(cmd), passTimeout,
				func(ctx context.Context, eng *engine.Engine) error {
					failed := false

					for _, decisionID := range ids {
						summary, err := act(ctx, eng, decisionID)
						if err != nil {
							_, _ = fmt.Fprintf(cmd.ErrOrStderr(), "%d: %v\n", decisionID, err)
							failed = true

							continue
						}

						_, _ = fmt.Fprintf(cmd.OutOrStdout(), "%d: %s\n", decisionID, summary)
					}

					if failed {
						return errReported
					}

					return nil
				})
		},
	)
}

func parseIDs(args []string) ([]int64, error) {
	ids := make([]int64, 0, len(args))

	for _, arg := range args {
		id, err := strconv.ParseInt(arg, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid decision ID %q: %w", arg, err)
		}

		ids = append(ids, id)
	}

	return ids, nil
}

func (a *app) statusCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show the configuration and what has been learnt",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withStats(
				cmd,
				func(ctx context.Context, cfg *config.Config, database *store.Store, stats store.Stats) error {
					table := tabwriter.NewWriter(cmd.OutOrStdout(), 0, 0, tablePadding, ' ', 0)
					_, _ = fmt.Fprintf(table, "Mode:\t%s\n", cfg.Policy.Mode)
					_, _ = fmt.Fprintf(
						table,
						"Account:\t%s on %s\n",
						cfg.Account.User,
						cfg.Account.Addr(),
					)
					_, _ = fmt.Fprintf(table, "Watching:\t%v\n", cfg.Folders.Watch)
					_, _ = fmt.Fprintf(
						table,
						"Training folders:\t%s, %s\n",
						cfg.Folders.TrainSpam,
						cfg.Folders.TrainGood,
					)
					_, _ = fmt.Fprintf(table, "Learnt:\t%d spam, %d good messages (%d tokens)\n",
						stats.Spam, stats.Good, stats.Tokens)
					_, _ = fmt.Fprintf(table, "Whitelist:\t%d addresses\n", stats.Addresses)
					_, _ = fmt.Fprintf(
						table,
						"Auth server id:\t%s\n",
						authServerIDLine(ctx, cfg, database),
					)
					_, _ = fmt.Fprintf(table, "State:\t%s\n", cfg.State)

					if err := table.Flush(); err != nil {
						return errors.Join(errReported, err)
					}

					return nil
				},
			)
		},
	}
}

// authServerIDLine says which authserv-id tamis pins Authentication-Results to,
// and where that came from.
func authServerIDLine(ctx context.Context, cfg *config.Config, database *store.Store) string {
	pin, err := engine.ResolveAuthPin(ctx, database, cfg)
	if err != nil {
		return "unknown: " + err.Error()
	}

	switch pin.Source {
	case engine.AuthStale:
		return fmt.Sprintf("not used: learnt for %s, the account now uses %s (run tamis bootstrap)",
			pin.LearntFor, pin.Host)
	case engine.AuthOff:
		return "off (configured)"
	case engine.AuthUnset:
		return "not set; only the topmost header counts (run tamis bootstrap to learn it)"
	case engine.AuthConfigured, engine.AuthLearnt:
		return fmt.Sprintf("%s (%s)", pin.ID, pin.Source)
	}

	return string(pin.Source)
}

// changeChecker is the part of a flag set chooseSources asks about.
type changeChecker interface {
	Changed(name string) bool
}

func anyChanged(flags changeChecker, names ...string) bool {
	return slices.ContainsFunc(names, flags.Changed)
}

// chooseSources fills opts from the flags. Naming any of --sent, --spam or
// --good limits bootstrap to what was named. Refreshing re-learns mail and
// leaves the whitelist and the watch position alone, unless --sent is named.
func chooseSources(
	cmd *cobra.Command,
	cfg *config.Config,
	opts *engine.BootstrapOptions,
	spam, good map[string]int,
) error {
	flags := cmd.Flags()
	named := anyChanged(flags, "sent", "spam", "good")
	opts.KeepCursors = named || opts.Refresh

	wantSpam, wantGood := !named || flags.Changed("spam"), !named || flags.Changed("good")
	if err := chooseFolders(cfg, opts, spam, good, wantSpam, wantGood); err != nil {
		return err
	}

	if (named || opts.Refresh) && !flags.Changed("sent") {
		opts.SentLimit = 0
	}

	return nil
}

// chooseFolders sets the spam and good folders bootstrap learns from, for
// the kinds that are wanted.
func chooseFolders(
	cfg *config.Config,
	opts *engine.BootstrapOptions,
	spam, good map[string]int,
	wantSpam, wantGood bool,
) error {
	var err error

	if wantSpam {
		if opts.Spam, err = toSources(spam, cfg.Folders.Junk); err != nil {
			return err
		}
	}

	if wantGood {
		if opts.Good, err = toSources(good, cfg.Folders.Watch[0]); err != nil {
			return err
		}
	}

	return nil
}

func printBootstrap(
	out io.Writer,
	cfg *config.Config,
	opts engine.BootstrapOptions,
	summary engine.BootstrapSummary,
) {
	if opts.SentLimit > 0 {
		_, _ = fmt.Fprintf(out, "Whitelisted %d addresses from %s.\n",
			summary.Addresses, cfg.Folders.Sent)
	}

	var trained []string
	if len(opts.Spam) > 0 {
		trained = append(trained, fmt.Sprintf("%d spam", summary.Spam))
	}

	if len(opts.Good) > 0 {
		trained = append(trained, fmt.Sprintf("%d good", summary.Good))
	}

	if len(trained) > 0 {
		_, _ = fmt.Fprintf(out, "Trained %s messages.\n", strings.Join(trained, " and "))
	}

	if summary.Refreshed > 0 {
		_, _ = fmt.Fprintf(
			out,
			"Refreshed %d messages that had been learnt before.\n",
			summary.Refreshed,
		)
	}

	if summary.AuthServerID != "" {
		_, _ = fmt.Fprintf(
			out,
			"Your provider's authentication results come from %s; set account.auth_server_id to override.\n",
			summary.AuthServerID,
		)
	}

	if !opts.KeepCursors {
		_, _ = fmt.Fprintf(out, "Watching %v from now on, in %s mode. Next: tamis run\n",
			cfg.Folders.Watch, cfg.Policy.Mode)
	}
}

func addBootstrapFlags(
	cmd *cobra.Command,
	opts *engine.BootstrapOptions,
	spam, good *map[string]int,
) {
	flags := cmd.Flags()
	flags.IntVar(
		&opts.SentLimit,
		"sent",
		defaultSentLimit,
		"recent sent messages to whitelist recipients from",
	)
	flags.StringToIntVarP(spam, "spam", "s", nil,
		"FOLDER=N: train the latest N messages as spam (default: junk folder=500)")
	flags.StringToIntVarP(good, "good", "g", nil,
		"FOLDER=N: train the latest N read messages as good (default: first watched folder=500)")
	flags.DurationVar(&opts.GoodMinAge, "good-min-age", defaultGoodMinAge,
		"only train read mail at least this old as good")
	flags.BoolVar(
		&opts.Refresh,
		"refresh",
		false,
		"learn already-learnt mail again with the current tokenizer; leaves the whitelist and watch position alone",
	)
}

var errBadSource = errors.New("want FOLDER=N with N > 0")

// toSources turns FOLDER=N flag values into bootstrap sources, in folder
// order, defaulting to fallback when none are given.
func toSources(values map[string]int, fallback string) ([]engine.Source, error) {
	if len(values) == 0 {
		return []engine.Source{{Folder: fallback, Limit: defaultCorpusSize}}, nil
	}

	sources := make([]engine.Source, 0, len(values))

	for _, folder := range slices.Sorted(maps.Keys(values)) {
		limit := values[folder]

		switch {
		case folder == "":
			return nil, fmt.Errorf("=%d: missing folder: %w", limit, errBadSource)
		case limit < 1:
			return nil, fmt.Errorf("%s=%d: count must be positive: %w", folder, limit, errBadSource)
		}

		sources = append(sources, engine.Source{Folder: folder, Limit: limit})
	}

	return sources, nil
}

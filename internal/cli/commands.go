package cli

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"text/tabwriter"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

const (
	defaultSentLimit  = 2000
	defaultCorpusSize = 500
	defaultGoodMinAge = 7 * 24 * time.Hour
	defaultLogLimit   = 20
	passTimeout       = 10 * time.Minute
	tablePadding      = 2
	stateDirMode      = 0o700
)

// app holds what the account-based commands share.
type app struct {
	env        Env
	configPath string
	finder     passwordFinder
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

func (a *app) connect(ctx context.Context, cfg *config.Config) (*mail.Client, error) {
	password, err := a.password(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("connect: %w", err)
	}

	client, err := mail.Dial(ctx, mail.DialConfig{
		Addr:      cfg.Account.Addr(),
		Username:  cfg.Account.User,
		Password:  password,
		TLSConfig: a.env.TLSConfig,
	})
	if err != nil {
		return nil, fmt.Errorf("connect: %w", err)
	}

	return client, nil
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
	body func(context.Context, *engine.Engine) error,
) error {
	client, err := a.connect(ctx, cfg)
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	ctx, cancel := context.WithTimeout(ctx, passTimeout)
	defer cancel()

	defer client.CloseWhenDone(ctx)()

	return body(ctx, engine.New(client, database, cfg, logger))
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
			ctx := cmd.Context()

			cfg, database, err := a.open(ctx)
			if err != nil {
				return err
			}

			defer func() { _ = database.Close() }()

			if opts.Spam, err = toSources(spam, cfg.Folders.Junk); err != nil {
				return err
			}

			if opts.Good, err = toSources(good, cfg.Folders.Watch[0]); err != nil {
				return err
			}

			var summary engine.BootstrapSummary

			err = a.withEngine(ctx, cfg, database, slog.New(slog.DiscardHandler),
				func(ctx context.Context, eng *engine.Engine) error {
					var err error

					summary, err = eng.Bootstrap(ctx, opts)

					return err //nolint:wrapcheck // engine errors carry their context
				})
			if err != nil {
				return err
			}

			_, _ = fmt.Fprintf(cmd.OutOrStdout(),
				"Whitelisted %d addresses from %s.\nTrained %d spam and %d good messages.\n"+
					"Watching %v from now on, in %s mode. Next: tamis run\n",
				summary.Addresses, cfg.Folders.Sent, summary.Spam, summary.Good,
				cfg.Folders.Watch, cfg.Policy.Mode)

			return nil
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
			ctx := cmd.Context()

			cfg, database, err := a.open(ctx)
			if err != nil {
				return err
			}

			defer func() { _ = database.Close() }()

			logger := slog.New(slog.NewTextHandler(cmd.ErrOrStderr(), nil))

			for {
				err := a.pass(ctx, cfg, database, logger)
				if once {
					return err
				}

				if err != nil {
					logger.ErrorContext(ctx, "pass failed", "err", err)
				}

				select {
				case <-ctx.Done():
					return nil
				case <-time.After(cfg.Policy.PollInterval.Duration):
				}
			}
		},
	}

	cmd.Flags().BoolVar(&once, "once", false, "make a single pass and exit")

	return cmd
}

func (a *app) pass(
	ctx context.Context,
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
) error {
	return a.withEngine(
		ctx,
		cfg,
		database,
		logger,
		func(ctx context.Context, eng *engine.Engine) error {
			summary, err := eng.RunOnce(ctx)
			if err != nil {
				return err //nolint:wrapcheck // engine errors carry their context
			}

			logger.InfoContext(ctx, "pass complete", "mode", cfg.Policy.Mode,
				"scanned", summary.Scanned, "spam", summary.Spam, "uncertain", summary.Uncertain,
				"good", summary.Good, "corrected", summary.Corrected, "trained", summary.Trained,
				"rescued", summary.Rescued)

			return nil
		},
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

			return writeDecisions(cmd, decisions)
		},
	}

	cmd.Flags().IntVarP(&limit, "number", "n", limit, "how many decisions to show")

	return cmd
}

func writeDecisions(cmd *cobra.Command, decisions []store.Decision) error {
	if len(decisions) == 0 {
		_, _ = fmt.Fprintln(cmd.OutOrStdout(), "No decisions yet.")

		return nil
	}

	table := tabwriter.NewWriter(cmd.OutOrStdout(), 0, 0, tablePadding, ' ', 0)
	_, _ = fmt.Fprintln(table, "ID\tTIME\tVERDICT\tREASON\tSCORE\tACTION\tFROM\tSUBJECT")

	for _, decision := range decisions {
		when := decision.At.Local() //nolint:gosmopolitan // a local CLI shows local time

		action := decision.Action
		if !decision.Applied && action != "none" {
			action = "would " + action
		}

		if decision.Undone {
			action += " (undone)"
		}

		_, _ = fmt.Fprintf(table, "%d\t%s\t%s\t%s\t%.2f\t%s\t%s\t%s\n",
			decision.ID,
			when.Format(time.DateTime),
			decision.Verdict,
			decision.Reason,
			decision.Score,
			action,
			decision.From,
			decision.Subject,
		)
	}

	if err := table.Flush(); err != nil {
		return fmt.Errorf("write log: %w", err)
	}

	return nil
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
			ctx := cmd.Context()

			ids, err := parseIDs(args)
			if err != nil {
				return err
			}

			cfg, database, err := a.open(ctx)
			if err != nil {
				return err
			}

			defer func() { _ = database.Close() }()

			return a.withEngine(ctx, cfg, database, slog.New(slog.DiscardHandler),
				func(ctx context.Context, eng *engine.Engine) error {
					return undoAll(ctx, cmd, eng, ids)
				})
		},
	}
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

// undoAll undoes each decision in turn, reporting each outcome, and fails if
// any of them did.
func undoAll(ctx context.Context, cmd *cobra.Command, eng *engine.Engine, ids []int64) error {
	failed := false

	for _, decisionID := range ids {
		summary, err := eng.Undo(ctx, decisionID)
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
}

func (a *app) statusCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show the configuration and what has been learnt",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			ctx := cmd.Context()

			cfg, database, err := a.open(ctx)
			if err != nil {
				return err
			}

			defer func() { _ = database.Close() }()

			stats, err := database.Stats(ctx)
			if err != nil {
				return err //nolint:wrapcheck // store errors carry their context
			}

			table := tabwriter.NewWriter(cmd.OutOrStdout(), 0, 0, tablePadding, ' ', 0)
			_, _ = fmt.Fprintf(table, "Mode:\t%s\n", cfg.Policy.Mode)
			_, _ = fmt.Fprintf(table, "Account:\t%s on %s\n", cfg.Account.User, cfg.Account.Addr())
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
			_, _ = fmt.Fprintf(table, "State:\t%s\n", cfg.State)

			if err := table.Flush(); err != nil {
				return errors.Join(errReported, err)
			}

			return nil
		},
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

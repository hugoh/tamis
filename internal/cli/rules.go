package cli

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/rules"
	"github.com/spf13/cobra"
)

var (
	errNoRules        = errors.New("no rules script: set rules.script in the config file")
	errMissingFolders = errors.New("rules file into folders that don't exist")
)

const folderCheckTimeout = time.Minute

func (a *app) rulesCommand() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "rules",
		Short: "View, validate and try the filing rules",
		Long: "Good mail is filed into folders by a Sieve script (RFC 5228) named by\n" +
			"rules.script. Only fileinto matters: tamis moves mail over IMAP.",
	}

	cmd.AddCommand(a.rulesShowCommand(), a.rulesCheckCommand(), a.rulesTestCommand())

	return cmd
}

func (a *app) rulesShowCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "show",
		Short: "Print the configured rules script",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			cfg, err := config.Load(a.env.Getenv, a.configPath)
			if err != nil {
				return err //nolint:wrapcheck // config errors already name the file
			}

			out := cmd.OutOrStdout()

			if cfg.Rules.Script == "" {
				_, _ = fmt.Fprintln(out, "No rules script configured (rules.script).")

				return nil
			}

			source, err := os.ReadFile(cfg.Rules.Script)
			if err != nil {
				return fmt.Errorf("read rules: %w", err)
			}

			_, _ = fmt.Fprintf(out, "# %s\n%s", cfg.Rules.Script, source)

			return nil
		},
	}
}

func (a *app) rulesCheckCommand() *cobra.Command {
	var folders bool

	cmd := &cobra.Command{
		Use:   "check [file]",
		Short: "Validate a Sieve script (default: rules.script)",
		Long: "Compile the script and report any error. With --folders, also connect to the\n" +
			"mailbox and fail if a folder the script files into doesn't exist.",
		Args: cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			path, err := a.rulesPath(args)
			if err != nil {
				return err
			}

			loaded, err := rules.Load(path)
			if err != nil {
				return fmt.Errorf("%s: %w", path, err)
			}

			_, _ = fmt.Fprintf(cmd.OutOrStdout(), "%s: ok\n", path)

			if !folders {
				return nil
			}

			return a.checkFolders(cmd, loaded.Targets())
		},
	}

	cmd.Flags().BoolVar(&folders, "folders", false,
		"also check the folders the script files into exist in the mailbox")

	return cmd
}

// checkFolders lists the mailbox and reports each target it lacks, with the
// nearest existing name when one is close.
func (a *app) checkFolders(cmd *cobra.Command, targets []string) error {
	cfg, err := config.Load(a.env.Getenv, a.configPath)
	if err != nil {
		return err //nolint:wrapcheck // config errors already name the file
	}

	ctx, cancel := context.WithTimeout(cmd.Context(), folderCheckTimeout)
	defer cancel()

	client, err := a.connect(ctx, cfg, a.logger(cmd))
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	existing, err := client.Folders()
	if err != nil {
		return fmt.Errorf("check folders: %w", err)
	}

	out := cmd.OutOrStdout()

	absent := rules.Missing(targets, existing)
	for _, folder := range absent {
		if folder.Nearest != "" {
			_, _ = fmt.Fprintf(
				out,
				"no such folder %q (did you mean %q?)\n",
				folder.Folder,
				folder.Nearest,
			)
		} else {
			_, _ = fmt.Fprintf(out, "no such folder %q\n", folder.Folder)
		}
	}

	if len(absent) > 0 {
		return fmt.Errorf("%w: %d folder(s)", errMissingFolders, len(absent))
	}

	_, _ = fmt.Fprintf(out, "all %d folders exist\n", len(targets))

	return nil
}

func (a *app) rulesTestCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "test <message.eml>",
		Short: "Show where the rules would file a message",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			path, err := a.rulesPath(nil)
			if err != nil {
				return err
			}

			loaded, err := rules.Load(path)
			if err != nil {
				return fmt.Errorf("%s: %w", path, err)
			}

			raw, err := os.ReadFile(args[0])
			if err != nil {
				return fmt.Errorf("read message: %w", err)
			}

			folder, err := loaded.Folder(cmd.Context(), raw, rules.SenderOf(raw))
			if err != nil {
				return fmt.Errorf("run rules: %w", err)
			}

			out := cmd.OutOrStdout()
			if folder == "" {
				_, _ = fmt.Fprintln(out, "stays in the inbox")
			} else {
				_, _ = fmt.Fprintf(out, "filed into %s\n", folder)
			}

			return nil
		},
	}
}

// rulesPath is the file argument, or the configured script when there is none.
func (a *app) rulesPath(args []string) (string, error) {
	if len(args) > 0 {
		return args[0], nil
	}

	cfg, err := config.Load(a.env.Getenv, a.configPath)
	if err != nil {
		return "", err //nolint:wrapcheck // config errors already name the file
	}

	if cfg.Rules.Script == "" {
		return "", errNoRules
	}

	return cfg.Rules.Script, nil
}

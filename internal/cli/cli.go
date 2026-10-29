// Package cli implements the tamis command-line interface.
package cli

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"io"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/secret"
	"github.com/spf13/cobra"
)

// Exit codes returned by Run.
const (
	exitOK   = 0
	exitFail = 1
	// exitUsage is a bad command line: nothing ran.
	exitUsage = 2
)

// errReported means the command already told the user what went wrong.
var errReported = errors.New("command failed")

// Env is the process environment Run works against.
type Env struct {
	Stdin  io.Reader
	Stdout io.Writer
	Stderr io.Writer
	Getenv func(string) string
	// TLSConfig, if set, replaces the default TLS settings for IMAP
	// connections. Tests use it to trust a throwaway certificate.
	TLSConfig *tls.Config
	// Prompt, if set, asks for a secret without echoing it. It returns
	// ErrNotTerminal when there is no terminal to ask on.
	Prompt func(prompt string) (string, error)
	// Keyring is the system keychain, or nil where there is none.
	Keyring secret.Keyring
}

// Run executes the command line in args and returns the process exit code.
func Run(ctx context.Context, args []string, env Env, version string) int {
	root, app := newRoot(env, version) //nolint:contextcheck // commands get ctx via cmd.Context()
	root.SetArgs(args)
	root.SetIn(env.Stdin)
	root.SetOut(env.Stdout)
	root.SetErr(env.Stderr)

	if err := root.ExecuteContext(ctx); err != nil {
		if !errors.Is(err, errReported) {
			_, _ = fmt.Fprintln(env.Stderr, "tamis:", err)
		}

		if !app.ran {
			return exitUsage
		}

		return exitFail
	}

	return exitOK
}

func newRoot(env Env, version string) (*cobra.Command, *app) {
	app := &app{env: env, finder: passwordFinder{env: env}}

	root := &cobra.Command{
		Use:           "tamis",
		Short:         "A second-opinion spam filter for remote mailboxes",
		Version:       version,
		SilenceErrors: true,
		SilenceUsage:  true,
	}
	root.SetVersionTemplate("tamis {{.Version}}\n")
	root.PersistentFlags().BoolVarP(&app.verbose, "verbose", "v", false, "log each IMAP exchange")
	root.PersistentFlags().StringVar(&app.color, "color", colorAuto,
		"colour output: auto (a terminal, unless NO_COLOR is set), always or never")
	root.PersistentFlags().
		StringVarP(&app.configPath, "config", "c", config.DefaultPath(env.Getenv),
			"configuration file")

	root.AddCommand(
		app.initCommand(),
		app.bootstrapCommand(),
		app.runCommand(),
		app.logCommand(),
		app.statusCommand(),
		app.doctorCommand(),
		app.undoCommand(),
		app.learnCommand(),
		app.loginCommand(),
		app.logoutCommand(),
		probeCommand(env),
		&cobra.Command{
			Use:   "version",
			Short: "Print the version",
			Args:  cobra.NoArgs,
			Run: func(cmd *cobra.Command, _ []string) {
				_, _ = fmt.Fprintln(cmd.OutOrStdout(), "tamis", version)
			},
		},
	)

	app.markRuns(root)

	return root, app
}

// markRuns makes app.ran true once a command starts running, so an error
// before that is a usage error: unknown command or flag, bad arguments, or
// a missing required flag.
func (a *app) markRuns(cmd *cobra.Command) {
	if run := cmd.RunE; run != nil {
		cmd.RunE = func(cmd *cobra.Command, args []string) error {
			a.ran = true

			return run(cmd, args)
		}
	}

	if run := cmd.Run; run != nil {
		cmd.Run = func(cmd *cobra.Command, args []string) {
			a.ran = true

			run(cmd, args)
		}
	}

	for _, sub := range cmd.Commands() {
		a.markRuns(sub)
	}
}

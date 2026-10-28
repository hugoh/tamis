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
	root := newRoot(env, version) //nolint:contextcheck // commands get ctx via cmd.Context()
	root.SetArgs(args)
	root.SetIn(env.Stdin)
	root.SetOut(env.Stdout)
	root.SetErr(env.Stderr)

	if err := root.ExecuteContext(ctx); err != nil {
		if !errors.Is(err, errReported) {
			_, _ = fmt.Fprintln(env.Stderr, "tamis:", err)
		}

		return exitFail
	}

	return exitOK
}

func newRoot(env Env, version string) *cobra.Command {
	app := &app{env: env, finder: passwordFinder{env: env}}

	root := &cobra.Command{
		Use:           "tamis",
		Short:         "A second-opinion spam filter for remote mailboxes",
		Version:       version,
		SilenceErrors: true,
		SilenceUsage:  true,
	}
	root.SetVersionTemplate("tamis {{.Version}}\n")
	root.PersistentFlags().
		StringVarP(&app.configPath, "config", "c", config.DefaultPath(env.Getenv),
			"configuration file")

	root.AddCommand(
		app.bootstrapCommand(),
		app.runCommand(),
		app.logCommand(),
		app.statusCommand(),
		app.undoCommand(),
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

	return root
}

// Package cli implements the tamis command-line interface.
package cli

import (
	"fmt"
	"io"
)

const usage = `tamis — a second-opinion spam filter for remote mailboxes

Usage:
  tamis <command>

Commands:
  version   Print the version
  help      Show this help
`

// Exit codes returned by Run.
const (
	exitOK    = 0
	exitUsage = 2
)

// Run executes the command named in args and returns the process exit code.
func Run(args []string, stdout, stderr io.Writer, version string) int {
	if len(args) == 0 {
		_, _ = fmt.Fprint(stderr, usage)

		return exitUsage
	}

	switch args[0] {
	case "version", "--version", "-v":
		_, _ = fmt.Fprintln(stdout, "tamis", version)

		return exitOK
	case "help", "--help", "-h":
		_, _ = fmt.Fprint(stdout, usage)

		return exitOK
	default:
		_, _ = fmt.Fprintf(stderr, "tamis: unknown command %q\n\n%s", args[0], usage)

		return exitUsage
	}
}

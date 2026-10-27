// Command tamis is a server-side spam filter that complements a mail
// provider's own filtering by watching remote IMAP/JMAP folders.
package main

import (
	"context"
	"os"
	"os/signal"
	"syscall"

	"github.com/hugoh/tamis/internal/cli"
)

// version is overridden at build time via -ldflags.
var version = "dev"

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)

	code := cli.Run(ctx, os.Args[1:], cli.Env{
		Stdin:  os.Stdin,
		Stdout: os.Stdout,
		Stderr: os.Stderr,
		Getenv: os.Getenv,
		Prompt: cli.TerminalPrompt(os.Stdin, os.Stderr),
	}, version)

	stop()
	os.Exit(code)
}

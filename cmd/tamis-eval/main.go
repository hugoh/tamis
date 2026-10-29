// Command tamis-eval is a development tool: it measures how tokenizer and
// feature changes affect tamis's filter on labelled mail, by progressive
// validation (see internal/eval). It is not part of the tamis binary.
//
//	tamis-eval fetch --spam Junk=1000 --ham Archive=2500 DIR
//	tamis-eval run DIR
//
// DIR holds ham/ and spam/ folders of messages. fetch makes a copy of your
// mail; keep it outside the repository and delete it when you are done.
package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/eval"
)

const (
	defaultRuns = 5
	usageText   = "usage: tamis-eval fetch|run [flags] DIR"
)

var errUsage = errors.New(usageText)

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, "tamis-eval:", err)
		os.Exit(1)
	}
}

func run(args []string, out, errOut io.Writer) error {
	if len(args) == 0 {
		return errUsage
	}

	switch args[0] {
	case "run":
		return runCommand(args[1:], out, errOut)
	case "fetch":
		return fetchCommand(args[1:], out, errOut)
	default:
		return fmt.Errorf("%w: unknown command %q", errUsage, args[0])
	}
}

func runCommand(args []string, out, errOut io.Writer) error {
	flags := flag.NewFlagSet("run", flag.ContinueOnError)
	flags.SetOutput(errOut)

	runs := flags.Int(
		"runs",
		defaultRuns,
		"runs per variant: the first in order, the others shuffled",
	)
	warmup := flags.Int("warmup", eval.DefaultWarmup, "leading messages learnt but not scored")
	own := flags.String("own", "", "comma-separated addresses mail is always expected on "+
		"(default: the tamis account's, if there is a config)")

	if err := flags.Parse(args); err != nil {
		return fmt.Errorf("run: %w", err)
	}

	if flags.NArg() != 1 {
		return fmt.Errorf("%w: run needs the folder holding ham/ and spam/", errUsage)
	}

	messages, err := eval.Load(flags.Arg(0))
	if err != nil {
		return fmt.Errorf("run: %w", err)
	}

	spam := 0

	for _, message := range messages {
		if message.Spam {
			spam++
		}
	}

	_, _ = fmt.Fprintf(out, "%d good and %d spam messages\n\n", len(messages)-spam, spam)

	variants := eval.Standard(ownAddresses(*own))

	results, err := eval.Compare(messages, variants, *runs, eval.Options{Warmup: *warmup})
	if err != nil {
		return fmt.Errorf("run: %w", err)
	}

	if err := eval.WriteReport(out, variants, results); err != nil {
		return fmt.Errorf("run: %w", err)
	}

	return nil
}

// ownAddresses is the --own list, or else the tamis account's address.
func ownAddresses(flagValue string) []string {
	if flagValue != "" {
		return strings.Split(flagValue, ",")
	}

	cfg, err := config.Load(os.Getenv, config.DefaultPath(os.Getenv))
	if err != nil {
		return nil
	}

	return []string{cfg.Account.User}
}

// Package cli implements the tamis command-line interface.
package cli

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/probe"
)

const usage = `tamis — a second-opinion spam filter for remote mailboxes

Usage:
  tamis <command> [flags]

Commands:
  probe     Check which IMAP features a server supports
  version   Print the version
  help      Show this help

Run 'tamis probe -h' for probe flags.
`

// Exit codes returned by Run.
const (
	exitOK    = 0
	exitFail  = 1
	exitUsage = 2
)

const (
	passwordEnv     = "TAMIS_PASSWORD"
	defaultPort     = 993
	defaultIdleWait = 30 * time.Second
)

// Env is the process environment Run works against.
type Env struct {
	Stdin  io.Reader
	Stdout io.Writer
	Stderr io.Writer
	Getenv func(string) string
}

// Run executes the command named in args and returns the process exit code.
func Run(args []string, env Env, version string) int {
	if len(args) == 0 {
		_, _ = fmt.Fprint(env.Stderr, usage)

		return exitUsage
	}

	switch args[0] {
	case "probe":
		return runProbe(args[1:], env)
	case "version", "--version", "-v":
		_, _ = fmt.Fprintln(env.Stdout, "tamis", version)

		return exitOK
	case "help", "--help", "-h":
		_, _ = fmt.Fprint(env.Stdout, usage)

		return exitOK
	default:
		_, _ = fmt.Fprintf(env.Stderr, "tamis: unknown command %q\n\n%s", args[0], usage)

		return exitUsage
	}
}

var errNoPassword = errors.New("no password: set " + passwordEnv + " or pipe it on stdin")

func runProbe(args []string, env Env) int {
	cfg, asJSON, code := parseProbeFlags(args, env)
	if code != exitOK || cfg == nil {
		return code
	}

	report, err := probe.Run(context.Background(), *cfg)
	if err != nil {
		_, _ = fmt.Fprintln(env.Stderr, "tamis probe:", err)

		return exitFail
	}

	if asJSON {
		err = report.WriteJSON(env.Stdout)
	} else {
		err = report.WriteText(env.Stdout)
	}

	if err != nil || report.Failed() {
		return exitFail
	}

	return exitOK
}

// parseProbeFlags returns the probe configuration, or a nil config and the
// exit code to stop with.
func parseProbeFlags(args []string, env Env) (*probe.Config, bool, int) {
	flags := flag.NewFlagSet("tamis probe", flag.ContinueOnError)
	flags.SetOutput(env.Stderr)
	flags.Usage = func() {
		_, _ = fmt.Fprintf(env.Stderr, "Usage: tamis probe --user ADDRESS [flags]\n\n"+
			"Reads the password (an app-specific password for iCloud) from $%s,\n"+
			"or from the first line of stdin.\n\n", passwordEnv)

		flags.PrintDefaults()
	}

	host := flags.String("host", "imap.mail.me.com", "IMAP server")
	port := flags.Int("port", defaultPort, "IMAP port (implicit TLS)")
	user := flags.String("user", "", "login name, usually the full address")
	write := flags.Bool("write", false, "also create, fill and delete scratch folders")
	asJSON := flags.Bool("json", false, "print the report as JSON")
	idleWait := flags.Duration("idle-wait", defaultIdleWait, "how long to wait for an IDLE push")
	plaintext := flags.Bool("plaintext", false, "connect without TLS (local test servers only)")

	if err := flags.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return nil, false, exitOK
		}

		return nil, false, exitUsage
	}

	if *user == "" {
		_, _ = fmt.Fprintln(env.Stderr, "tamis probe: --user is required")

		flags.Usage()

		return nil, false, exitUsage
	}

	password, err := readPassword(env)
	if err != nil {
		_, _ = fmt.Fprintln(env.Stderr, "tamis probe:", err)

		return nil, false, exitUsage
	}

	return &probe.Config{
		Addr:      net.JoinHostPort(*host, strconv.Itoa(*port)),
		Username:  *user,
		Password:  password,
		Write:     *write,
		IdleWait:  *idleWait,
		Plaintext: *plaintext,
	}, *asJSON, exitOK
}

func readPassword(env Env) (string, error) {
	if password := env.Getenv(passwordEnv); password != "" {
		return password, nil
	}

	line, err := bufio.NewReader(env.Stdin).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("read password: %w", err)
	}

	password := strings.TrimRight(line, "\r\n")
	if password == "" {
		return "", errNoPassword
	}

	return password, nil
}

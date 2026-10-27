package cli

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/probe"
	"github.com/spf13/cobra"
)

const (
	passwordEnv     = "TAMIS_PASSWORD"
	defaultPort     = 993
	defaultIdleWait = 30 * time.Second
)

var errNoPassword = errors.New("no password: set " + passwordEnv + " or pipe it on stdin")

func probeCommand(env Env) *cobra.Command {
	var (
		cfg    probe.Config
		host   string
		port   int
		asJSON bool
	)

	cmd := &cobra.Command{
		Use:   "probe",
		Short: "Check which IMAP features a server supports",
		Long: "Check which IMAP features a server supports.\n\n" +
			"Reads the password (an app-specific password for iCloud) from $" + passwordEnv + ",\n" +
			"or from the first line of stdin.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			password, err := readPassword(env, cmd.InOrStdin(), cfg.Username)
			if err != nil {
				return err
			}

			cfg.Addr = net.JoinHostPort(host, strconv.Itoa(port))
			cfg.Password = password
			cfg.TLSConfig = env.TLSConfig

			report, err := probe.Run(cmd.Context(), cfg)
			if err != nil {
				return fmt.Errorf("probe: %w", err)
			}

			if asJSON {
				err = report.WriteJSON(cmd.OutOrStdout())
			} else {
				err = report.WriteText(cmd.OutOrStdout())
			}

			if err != nil {
				return fmt.Errorf("probe: %w", err)
			}

			if report.Failed() {
				return errReported
			}

			return nil
		},
	}

	addProbeFlags(cmd, &cfg, &host, &port, &asJSON)

	return cmd
}

func addProbeFlags(cmd *cobra.Command, cfg *probe.Config, host *string, port *int, asJSON *bool) {
	flags := cmd.Flags()
	flags.StringVar(host, "host", "", "IMAP server, e.g. imap.mail.me.com for iCloud")
	flags.IntVar(port, "port", defaultPort, "IMAP port (implicit TLS)")
	flags.StringVar(&cfg.Username, "user", "", "login name, usually the full address")
	flags.BoolVar(&cfg.Write, "write", false, "also create, fill and delete scratch folders")
	flags.BoolVar(asJSON, "json", false, "print the report as JSON")
	flags.DurationVar(
		&cfg.IdleWait,
		"idle-wait",
		defaultIdleWait,
		"how long to wait for an IDLE push",
	)
	flags.BoolVar(
		&cfg.Plaintext,
		"plaintext",
		false,
		"connect without TLS (local test servers only)",
	)

	_ = cmd.MarkFlagRequired("host")
	_ = cmd.MarkFlagRequired("user")
}

// readPassword takes the password from $TAMIS_PASSWORD, a terminal prompt,
// or the first line of stdin, in that order.
func readPassword(env Env, stdin io.Reader, user string) (string, error) {
	if password := env.Getenv(passwordEnv); password != "" {
		return password, nil
	}

	if password, asked, err := promptPassword(env, user); asked {
		return password, err
	}

	line, err := bufio.NewReader(stdin).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("read password: %w", err)
	}

	password := strings.TrimRight(line, "\r\n")
	if password == "" {
		return "", errNoPassword
	}

	return password, nil
}

// promptPassword asks on the terminal. asked is false when there is no
// terminal to ask on.
func promptPassword(env Env, user string) (string, bool, error) {
	if env.Prompt == nil {
		return "", false, nil
	}

	password, err := env.Prompt(fmt.Sprintf("Password for %s: ", user))

	switch {
	case errors.Is(err, ErrNotTerminal):
		return "", false, nil
	case err != nil:
		return "", true, fmt.Errorf("read password: %w", err)
	case password == "":
		return "", true, errNoPassword
	default:
		return password, true, nil
	}
}

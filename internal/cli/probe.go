package cli

import (
	"fmt"
	"net"
	"strconv"
	"time"

	"github.com/hugoh/tamis/internal/probe"
	"github.com/spf13/cobra"
)

const (
	defaultPort     = 993
	defaultIdleWait = 30 * time.Second
)

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
			"The password (an app-specific one for iCloud) comes from the system keychain\n" +
			"if 'tamis login' stored one for this host and user, then $" + passwordEnv + ", then a\n" +
			"prompt, or the first line of stdin when there is no terminal.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			finder := &passwordFinder{env: env, stdin: cmd.InOrStdin()}

			password, err := finder.find(cmd.Context(), credentials{host: host, user: cfg.Username})
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

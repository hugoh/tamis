package cli

import (
	"errors"
	"fmt"
	"net"
	"net/netip"
	"strconv"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/probe"
	"github.com/spf13/cobra"
)

const (
	defaultPort     = 993
	defaultIdleWait = 30 * time.Second
)

var errPlaintextRemote = errors.New(
	"--plaintext sends the password unencrypted, so it only connects to loopback hosts",
)

// isLoopback reports whether host is localhost or a loopback address.
func isLoopback(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}

	ip, err := netip.ParseAddr(host)

	return err == nil && ip.IsLoopback()
}

func (a *app) probeCommand() *cobra.Command {
	var (
		cfg    probe.Config
		host   string
		port   int
		asJSON bool
	)

	cmd := &cobra.Command{
		Use:   "probe",
		Short: "Check which IMAP features a server supports",
		Long: "Check which IMAP features a server supports, and list its folders.\n\n" +
			"--host, --user and --port default to the account in the configuration file.\n" +
			"The password (an app-specific one for iCloud) comes from the system keychain\n" +
			"if 'tamis login' stored one for this host and user, then $" + passwordEnv + ", then a\n" +
			"prompt, or the first line of stdin when there is no terminal.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.runProbe(cmd, &cfg, host, port, asJSON)
		},
	}

	addProbeFlags(cmd, &cfg, &host, &port, &asJSON)

	return cmd
}

func (a *app) runProbe(
	cmd *cobra.Command,
	cfg *probe.Config,
	host string,
	port int,
	asJSON bool,
) error {
	login, err := a.probeLogin(cmd, &host, &port, cfg)
	if err != nil {
		return err
	}

	if cfg.Plaintext && !isLoopback(host) {
		return fmt.Errorf("probe: %w", errPlaintextRemote)
	}

	finder := &passwordFinder{env: a.env, stdin: cmd.InOrStdin()}

	password, err := finder.find(cmd.Context(), login)
	if err != nil {
		return err
	}

	cfg.Addr = net.JoinHostPort(host, strconv.Itoa(port))
	cfg.Password = password
	cfg.TLSConfig = a.env.TLSConfig

	report, err := probe.Run(*cfg)
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
}

// probeLogin fills in what --host, --user and --port left out from the
// configured account. The account's password_command only applies when the
// server and user are the account's own.
func (a *app) probeLogin(
	cmd *cobra.Command,
	host *string,
	port *int,
	probeCfg *probe.Config,
) (credentials, error) {
	if *host != "" && probeCfg.Username != "" {
		return credentials{host: *host, user: probeCfg.Username}, nil
	}

	loaded, err := config.Load(a.env.Getenv, a.configPath)
	if err != nil {
		return credentials{}, fmt.Errorf(
			"probe: give --host and --user, or a configuration: %w",
			err,
		)
	}

	account := loaded.Account
	if *host == "" {
		*host = account.Host
	}

	if probeCfg.Username == "" {
		probeCfg.Username = account.User
	}

	if !cmd.Flags().Changed("port") {
		*port = account.Port
	}

	login := credentials{host: *host, user: probeCfg.Username}
	if *host == account.Host && probeCfg.Username == account.User {
		login.command = account.PasswordCommand
	}

	return login, nil
}

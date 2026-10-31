package cli

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/store"
	"github.com/hugoh/tamis/internal/web"
	"github.com/spf13/cobra"
)

const (
	enrollTTL   = 15 * time.Minute
	logRingSize = 1000
)

var errNoTLS = errors.New(
	"web is not serving TLS: set self_signed = true, or tls_cert and tls_key, in [web]",
)

var errWebDisabled = errors.New(
	"web is not enabled: set enabled = true and origin in the [web] section",
)

func (a *app) webCommand() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "web",
		Short: "Manage passkeys for the web interface",
		Long: "The web interface (the [web] config section, served by `tamis run`) signs in\n" +
			"with passkeys. Enroll the first one with `tamis web enroll`.",
	}

	cmd.AddCommand(
		a.webEnrollCommand(),
		a.webPasskeysCommand(),
		a.webRevokeCommand(),
		a.webCertCommand(),
	)

	return cmd
}

func (a *app) withWeb(
	cmd *cobra.Command,
	body func(context.Context, *config.Config, *store.Store) error,
) error {
	return a.withStore(
		cmd,
		func(ctx context.Context, cfg *config.Config, database *store.Store) error {
			if !cfg.Web.Enabled {
				return errWebDisabled
			}

			return body(ctx, cfg, database)
		},
	)
}

func (a *app) webEnrollCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "enroll",
		Short: "Print a one-time link to enroll a passkey",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withWeb(
				cmd,
				func(ctx context.Context, cfg *config.Config, database *store.Store) error {
					token, err := web.NewToken()
					if err != nil {
						return err //nolint:wrapcheck // the error says what failed
					}

					err = database.AddEnrollment(
						ctx,
						web.HashToken(token),
						time.Now().Add(enrollTTL),
					)
					if err != nil {
						return err //nolint:wrapcheck // store errors carry their context
					}

					_, _ = fmt.Fprintf(
						cmd.OutOrStdout(),
						"Open this link within %d minutes to enroll a passkey; it works once:\n%s/auth/enroll?token=%s\n",
						int(enrollTTL.Minutes()),
						strings.TrimSuffix(cfg.Web.Origin, "/"),
						token,
					)

					return nil
				},
			)
		},
	}
}

func (a *app) webPasskeysCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "passkeys",
		Short: "List enrolled passkeys",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withWeb(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					keys, err := database.Passkeys(ctx)
					if err != nil {
						return err //nolint:wrapcheck // store errors carry their context
					}

					return a.writePasskeys(cmd, keys)
				},
			)
		},
	}
}

func (a *app) writePasskeys(cmd *cobra.Command, keys []store.Passkey) error {
	out := cmd.OutOrStdout()

	if len(keys) == 0 {
		_, _ = fmt.Fprintln(out, "No passkeys enrolled. Run `tamis web enroll` to add one.")

		return nil
	}

	colors, err := a.paletteFor(out)
	if err != nil {
		return err
	}

	text := func(value string, s style) cell { return cell{{value, s}} }

	rows := [][]cell{{
		text("ID", styleBold), text("NAME", styleBold),
		text("ADDED", styleBold), text("LAST USED", styleBold),
	}}

	for _, key := range keys {
		lastUsed := "never"
		if !key.LastUsed.IsZero() {
			lastUsed = localTime(key.LastUsed)
		}

		rows = append(rows, []cell{
			text(strconv.FormatInt(key.ID, 10), ""),
			text(key.Name, ""),
			text(localTime(key.CreatedAt), styleDim),
			text(lastUsed, styleDim),
		})
	}

	return writeTable(out, colors, rows)
}

func (a *app) webRevokeCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "revoke ID...",
		Short: "Remove enrolled passkeys",
		Args:  cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			ids := make([]int64, 0, len(args))

			for _, arg := range args {
				id, err := strconv.ParseInt(arg, 10, 64)
				if err != nil {
					return fmt.Errorf("invalid passkey ID %q: %w", arg, err)
				}

				ids = append(ids, id)
			}

			return a.withWeb(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					for _, id := range ids {
						if err := database.RemovePasskey(ctx, id); err != nil {
							return err //nolint:wrapcheck // store errors carry their context
						}

						_, _ = fmt.Fprintf(cmd.OutOrStdout(), "revoked passkey %d\n", id)
					}

					return nil
				},
			)
		},
	}
}

func localTime(when time.Time) string {
	return when.Local().Format(time.DateTime) //nolint:gosmopolitan // a local CLI shows local time
}

// startWeb binds the web interface, if enabled, and serves it until the
// returned function is called. Binding happens first so a taken port stops
// `run` before it touches the mailbox.
func (a *app) startWeb(
	ctx context.Context,
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	status *live.Status,
	logs *live.Ring,
) (func(), error) {
	if !cfg.Web.Enabled {
		return func() {}, nil
	}

	srv, err := web.New(
		cfg,
		database,
		logger,
		web.Options{
			Run:     a.engineRunner(cfg, database, logger),
			Status:  status,
			Logs:    logs,
			Version: a.version,
		},
	)
	if err != nil {
		return nil, fmt.Errorf("web interface: %w", err)
	}

	listener, listening, err := listenWeb(ctx, cfg)
	if err != nil {
		return nil, err
	}

	serveCtx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})

	go func() {
		defer close(done)

		if err := srv.Serve(serveCtx, listener); err != nil {
			logger.ErrorContext(ctx, "web interface stopped", "err", err)
		}
	}()

	logger.InfoContext(ctx, "web interface listening", listening...)

	return func() {
		cancel()
		<-done
	}, nil
}

// listenWeb binds the web interface's address, wrapped in TLS when configured,
// and says what to log about it.
func listenWeb(ctx context.Context, cfg *config.Config) (net.Listener, []any, error) {
	certFile, keyFile, err := tlsFiles(cfg)
	if err != nil {
		return nil, nil, err
	}

	var lc net.ListenConfig

	listener, err := lc.Listen(ctx, "tcp", cfg.Web.Listen)
	if err != nil {
		return nil, nil, fmt.Errorf("web interface: %w", err)
	}

	listening := []any{"addr", listener.Addr().String(), "origin", cfg.Web.Origin}

	if certFile == "" {
		return listener, listening, nil
	}

	return withTLS(listener, listening, certFile, keyFile)
}

// tlsFiles is the certificate and key the web interface serves HTTPS with, or
// empty strings when it serves plain HTTP. A self-signed pair is made on first
// use.
func tlsFiles(cfg *config.Config) (string, string, error) {
	switch {
	case cfg.Web.SelfSigned:
		certFile, keyFile, err := web.SelfSigned(
			filepath.Dir(cfg.State),
			cfg.Web.RPID(),
			time.Now(),
		)
		if err != nil {
			return "", "", fmt.Errorf("web interface: %w", err)
		}

		return certFile, keyFile, nil
	case cfg.Web.TLSCert != "":
		return cfg.Web.TLSCert, cfg.Web.TLSKey, nil
	}

	return "", "", nil
}

func withTLS(
	listener net.Listener,
	listening []any,
	certFile, keyFile string,
) (net.Listener, []any, error) {
	secured, err := web.TLSListener(listener, certFile, keyFile)
	if err != nil {
		_ = listener.Close()

		return nil, nil, fmt.Errorf("web interface: %w", err)
	}

	fingerprint, err := web.Fingerprint(certFile)
	if err != nil {
		_ = listener.Close()

		return nil, nil, fmt.Errorf("web interface: %w", err)
	}

	return secured, append(listening, "fingerprint", fingerprint), nil
}

func (a *app) webCertCommand() *cobra.Command {
	var asPEM bool

	cmd := &cobra.Command{
		Use:   "cert",
		Short: "Show the certificate to install on your devices",
		Long: "Show the certificate the web interface serves HTTPS with, making a self-signed\n" +
			"one first if web.self_signed is set. Browsers only accept passkeys from a\n" +
			"certificate they trust, so install it on each device. --pem prints the\n" +
			"certificate itself, never the key.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withWeb(
				cmd,
				func(_ context.Context, cfg *config.Config, _ *store.Store) error {
					certFile, _, err := tlsFiles(cfg)
					if err != nil {
						return err
					}

					if certFile == "" {
						return errNoTLS
					}

					return writeCert(cmd, certFile, asPEM)
				},
			)
		},
	}

	cmd.Flags().
		BoolVar(&asPEM, "pem", false, "print the certificate as PEM, for copying to a device")

	return cmd
}

func writeCert(cmd *cobra.Command, certFile string, asPEM bool) error {
	out := cmd.OutOrStdout()

	if asPEM {
		data, err := os.ReadFile(certFile) //nolint:gosec // the user's certificate
		if err != nil {
			return fmt.Errorf("read certificate: %w", err)
		}

		_, _ = out.Write(data)

		return nil
	}

	fingerprint, err := web.Fingerprint(certFile)
	if err != nil {
		return fmt.Errorf("fingerprint: %w", err)
	}

	_, _ = fmt.Fprintf(out, "Certificate: %s\nSHA-256 fingerprint: %s\n\n"+
		"Copy it to each device (tamis web cert --pem) and trust it for web sites;\n"+
		"until then its browser will refuse to create or use a passkey.\n", certFile, fingerprint)

	return nil
}

// engineRunner gives the web interface a way to connect to the mailbox for
// undo and learn, with the same connection and password handling as the
// command-line commands.
func (a *app) engineRunner(
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
) web.RunActions {
	return func(ctx context.Context, body func(context.Context, web.Actions) error) error {
		return a.withEngine(ctx, cfg, database, logger, passTimeout,
			func(ctx context.Context, eng *engine.Engine) error { return body(ctx, eng) })
	}
}

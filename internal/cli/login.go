package cli

import (
	"errors"
	"fmt"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/secret"
	"github.com/spf13/cobra"
)

const keychainAlternatives = "use account.password_command, or a systemd credential, instead"

func (a *app) loginCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "login",
		Short: "Check the account password and keep it in the system keychain",
		Long: "Ask for the account password (an app-specific one for iCloud), check it\n" +
			"against the server, and store it in the system keychain: the macOS Keychain,\n" +
			"the Secret Service (GNOME Keyring, KWallet) or the Windows Credential Manager.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			cfg, err := a.keychainConfig()
			if errors.Is(err, errNoKeychain) {
				return fmt.Errorf("%w; %s", err, keychainAlternatives)
			}

			if err != nil {
				return err
			}

			password, err := typePassword(a.env, cmd.InOrStdin(), cfg.Account.User)
			if err != nil {
				return err
			}

			if err := a.checkPassword(cmd, cfg, password); err != nil {
				return err
			}

			err = a.env.Keyring.Set(secret.Service(cfg.Account.Host), cfg.Account.User, password)
			if err != nil {
				return fmt.Errorf("store the password: %w; %s", err, keychainAlternatives)
			}

			_, _ = fmt.Fprintf(
				cmd.OutOrStdout(),
				"Saved the password for %s on %s in the system keychain.\n",
				cfg.Account.User,
				cfg.Account.Host,
			)

			return nil
		},
	}
}

// keychainConfig loads the configuration, provided there is a keychain to use.
func (a *app) keychainConfig() (*config.Config, error) {
	cfg, err := config.Load(a.env.Getenv, a.configPath)
	if err != nil {
		return nil, err //nolint:wrapcheck // config errors already name the file
	}

	if a.env.Keyring == nil {
		return nil, errNoKeychain
	}

	return cfg, nil
}

func (a *app) checkPassword(cmd *cobra.Command, cfg *config.Config, password string) error {
	client, err := mail.Dial(cmd.Context(), mail.DialConfig{
		Addr:      cfg.Account.Addr(),
		Username:  cfg.Account.User,
		Password:  password,
		TLSConfig: a.env.TLSConfig,
	})
	if err != nil {
		return fmt.Errorf("check the password: %w", err)
	}

	return client.Close() //nolint:wrapcheck // logging out is all that's left
}

func (a *app) logoutCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "logout",
		Short: "Remove the account password from the system keychain",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			cfg, err := a.keychainConfig()
			if err != nil {
				return err
			}

			err = a.env.Keyring.Delete(secret.Service(cfg.Account.Host), cfg.Account.User)

			switch {
			case errors.Is(err, secret.ErrNotFound):
				_, _ = fmt.Fprintf(cmd.OutOrStdout(), "No password stored for %s on %s.\n",
					cfg.Account.User, cfg.Account.Host)
			case err != nil:
				return fmt.Errorf("remove the password: %w", err)
			default:
				_, _ = fmt.Fprintf(
					cmd.OutOrStdout(),
					"Removed the password for %s on %s from the system keychain.\n",
					cfg.Account.User,
					cfg.Account.Host,
				)
			}

			return nil
		},
	}
}

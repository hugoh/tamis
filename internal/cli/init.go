package cli

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"

	"github.com/pelletier/go-toml/v2"
	"github.com/spf13/cobra"
)

// Owner-only, because password_command in the config runs what it names.
const (
	dirMode  = 0o700
	fileMode = 0o600
)

// errConfigExists means init would overwrite a configuration file.
var errConfigExists = errors.New("config already exists; edit it instead")

func (a *app) initCommand() *cobra.Command {
	var host, user string

	cmd := &cobra.Command{
		Use:   "init",
		Short: "Write a starter configuration file",
		Long: "Write a configuration file naming the account. Every other setting has a\n" +
			"default. It refuses to overwrite an existing file.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			if err := writeNewConfig(a.configPath, host, user); err != nil {
				return err
			}

			_, _ = fmt.Fprintf(cmd.OutOrStdout(),
				"Wrote %s.\nNext: tamis login, to store the password in the system keychain.\n",
				a.configPath)

			return nil
		},
	}

	cmd.Flags().StringVar(&host, "host", "", "IMAP server, e.g. imap.mail.me.com for iCloud")
	cmd.Flags().StringVar(&user, "user", "", "login name, usually the full address")
	_ = cmd.MarkFlagRequired("host")
	_ = cmd.MarkFlagRequired("user")

	return cmd
}

func writeNewConfig(path, host, user string) error {
	data, err := toml.Marshal(map[string]any{
		"account": map[string]string{"host": host, "user": user},
	})
	if err != nil {
		return fmt.Errorf("encode config: %w", err)
	}

	if err := os.MkdirAll(filepath.Dir(path), dirMode); err != nil {
		return fmt.Errorf("create config directory: %w", err)
	}

	file, err := os.OpenFile( //nolint:gosec // the config path is the user's to choose
		path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, fileMode,
	)
	if errors.Is(err, os.ErrExist) {
		return fmt.Errorf("%w: %s", errConfigExists, path)
	}

	if err != nil {
		return fmt.Errorf("create config: %w", err)
	}

	if _, err := file.Write(data); err != nil {
		_ = file.Close()

		return fmt.Errorf("write config: %w", err)
	}

	if err := file.Close(); err != nil {
		return fmt.Errorf("write config: %w", err)
	}

	return nil
}

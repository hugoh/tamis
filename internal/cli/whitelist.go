package cli

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

const manualSource = "manual"

var errNotWhitelisted = errors.New("not whitelisted")

func (a *app) whitelistCommand() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "whitelist",
		Short: "View and edit the whitelist",
		Long: "The whitelist holds senders tamis trusts. Recipients you write to are\n" +
			"added automatically; these commands add and remove addresses by hand.",
	}

	cmd.AddCommand(a.whitelistListCommand(), a.whitelistAddCommand(), a.whitelistRemoveCommand())

	return cmd
}

func (a *app) whitelistListCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "list",
		Short: "List whitelisted addresses",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					addresses, err := database.Addresses(ctx)
					if err != nil {
						return err //nolint:wrapcheck // store errors carry their context
					}

					return a.writeAddresses(cmd, addresses)
				},
			)
		},
	}
}

func (a *app) writeAddresses(cmd *cobra.Command, addresses []store.Address) error {
	out := cmd.OutOrStdout()

	if len(addresses) == 0 {
		_, _ = fmt.Fprintln(out, "No addresses whitelisted.")

		return nil
	}

	colors, err := a.paletteFor(out)
	if err != nil {
		return err
	}

	text := func(value string, s style) cell { return cell{{value, s}} }

	rows := [][]cell{
		{text("ADDRESS", styleBold), text("SOURCE", styleBold), text("ADDED", styleBold)},
	}

	for _, address := range addresses {
		added := address.AddedAt.Local() //nolint:gosmopolitan // a local CLI shows local time

		rows = append(rows, []cell{
			text(address.Address, ""),
			text(address.Source, styleDim),
			text(added.Format(time.DateTime), styleDim),
		})
	}

	return writeTable(out, colors, rows)
}

func (a *app) whitelistAddCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "add ADDRESS...",
		Short: "Whitelist addresses",
		Args:  cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					for _, arg := range args {
						address, err := store.ParseAddress(arg)
						if err != nil {
							return err //nolint:wrapcheck // the error names the address
						}

						if err := database.AddAddress(ctx, address, manualSource); err != nil {
							return err //nolint:wrapcheck // store errors carry their context
						}

						_, _ = fmt.Fprintf(cmd.OutOrStdout(), "whitelisted %s\n", address)
					}

					return nil
				},
			)
		},
	}
}

func (a *app) whitelistRemoveCommand() *cobra.Command {
	return &cobra.Command{
		Use:     "rm ADDRESS...",
		Aliases: []string{"remove"},
		Short:   "Remove addresses from the whitelist",
		Args:    cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					for _, arg := range args {
						address, err := store.ParseAddress(arg)
						if err != nil {
							return err //nolint:wrapcheck // the error names the address
						}

						found, err := database.HasAddress(ctx, address)
						if err != nil {
							return err //nolint:wrapcheck // store errors carry their context
						}

						if !found {
							return fmt.Errorf("%s is %w", address, errNotWhitelisted)
						}

						if err := database.RemoveAddress(ctx, address); err != nil {
							return err //nolint:wrapcheck // store errors carry their context
						}

						_, _ = fmt.Fprintf(cmd.OutOrStdout(), "removed %s\n", address)
					}

					return nil
				},
			)
		},
	}
}

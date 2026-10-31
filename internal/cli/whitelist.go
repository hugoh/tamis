package cli

import (
	"context"
	"errors"
	"fmt"
	"math"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

const manualSource = "manual"

var (
	errNotWhitelisted       = errors.New("not whitelisted")
	errAddressesAndMatching = errors.New("give either addresses or --matching, not both")
)

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
	var (
		matching string
		confirm  bool
	)

	cmd := &cobra.Command{
		Use:     "rm ADDRESS... | --matching TEXT [--yes]",
		Aliases: []string{"remove"},
		Short:   "Remove addresses from the whitelist",
		Long: "Remove the given addresses, or with --matching every address or source that\n" +
			"contains TEXT. --matching lists what it would remove and does nothing more\n" +
			"until you repeat it with --yes.",
		Args: func(cmd *cobra.Command, args []string) error {
			if matching == "" {
				return cobra.MinimumNArgs(1)(cmd, args)
			}

			if len(args) > 0 {
				return errAddressesAndMatching
			}

			return nil
		},
		RunE: func(cmd *cobra.Command, args []string) error {
			return a.withStore(
				cmd,
				func(ctx context.Context, _ *config.Config, database *store.Store) error {
					if matching != "" {
						return a.removeMatching(ctx, cmd, database, matching, confirm)
					}

					return removeListed(ctx, cmd, database, args)
				},
			)
		},
	}

	cmd.Flags().
		StringVar(&matching, "matching", "", "remove every address or source containing this text")
	cmd.Flags().BoolVar(&confirm, "yes", false, "with --matching, really remove them")

	return cmd
}

// removeListed removes the named addresses, once all of them have been checked.
func removeListed(
	ctx context.Context,
	cmd *cobra.Command,
	database *store.Store,
	args []string,
) error {
	addresses := make([]string, 0, len(args))

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

		addresses = append(addresses, address)
	}

	if _, err := database.RemoveAddresses(ctx, addresses); err != nil {
		return err //nolint:wrapcheck // store errors carry their context
	}

	for _, address := range addresses {
		_, _ = fmt.Fprintf(cmd.OutOrStdout(), "removed %s\n", address)
	}

	return nil
}

// removeMatching shows the addresses a search matches and, if confirm is set,
// removes them.
func (a *app) removeMatching(
	ctx context.Context,
	cmd *cobra.Command,
	database *store.Store,
	search string,
	confirm bool,
) error {
	out := cmd.OutOrStdout()

	found, total, err := database.QueryAddresses(
		ctx,
		store.AddressQuery{Search: search, Limit: math.MaxInt32},
	)
	if err != nil {
		return err //nolint:wrapcheck // store errors carry their context
	}

	if total == 0 {
		_, _ = fmt.Fprintln(out, "No addresses match.")

		return nil
	}

	if !confirm {
		if err := a.writeAddresses(cmd, found); err != nil {
			return err
		}

		_, _ = fmt.Fprintf(out, "\n%s; run again with --yes to remove them.\n", matchLine(total))

		return nil
	}

	removed, err := database.RemoveAddressesMatching(ctx, search)
	if err != nil {
		return err //nolint:wrapcheck // store errors carry their context
	}

	noun := "addresses"
	if removed == 1 {
		noun = "address"
	}

	_, _ = fmt.Fprintf(out, "removed %d %s\n", removed, noun)

	return nil
}

func matchLine(total int) string {
	if total == 1 {
		return "1 address matches"
	}

	return fmt.Sprintf("%d addresses match", total)
}

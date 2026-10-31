package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

func (a *app) doctorCommand() *cobra.Command {
	var asJSON bool

	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Check the configuration, the rules and that the filter is in good shape",
		Long: "Check that the configuration loads and the filing rules compile, that the filter\n" +
			"has learnt from both kinds of mail, how it has been deciding lately, and whether\n" +
			"it is still deciding. Exits non-zero when a check fails.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			if _, err := config.Load(a.env.Getenv, a.configPath); err != nil {
				var inRules *config.RulesError

				return a.writeHealth(
					cmd,
					health.ConfigFailed(err, errors.As(err, &inRules)),
					asJSON,
				)
			}

			return a.withStore(
				cmd,
				func(ctx context.Context, cfg *config.Config, database *store.Store) error {
					report, err := health.Collect(ctx, database, cfg)
					if err != nil {
						return err //nolint:wrapcheck // store errors carry their context
					}

					return a.writeHealth(cmd, report, asJSON)
				},
			)
		},
	}

	cmd.Flags().BoolVar(&asJSON, "json", false, "print the report as JSON")

	return cmd
}

// writeHealth prints the report, and fails the command if a check failed.
func (a *app) writeHealth(cmd *cobra.Command, report *health.Report, asJSON bool) error {
	out := cmd.OutOrStdout()

	var err error

	if asJSON {
		err = report.WriteJSON(out)
	} else {
		err = a.writeHealthText(out, report)
	}

	if err != nil {
		return errors.Join(errReported, err)
	}

	if report.Failed() {
		return errReported
	}

	return nil
}

var statusStyles = map[health.Status]style{ //nolint:gochecknoglobals // constant lookup
	health.StatusOK:   styleGreen,
	health.StatusWarn: styleYellow,
	health.StatusFail: styleRed,
	health.StatusSkip: styleDim,
}

// writeHealthText prints the checks under a heading for each section, with
// problems in capitals and in colour.
func (a *app) writeHealthText(out io.Writer, report *health.Report) error {
	colors, err := a.paletteFor(out)
	if err != nil {
		return err
	}

	var rows [][]cell

	section := ""

	for _, check := range report.Checks {
		if check.Section != section {
			if section != "" {
				rows = append(rows, []cell{{}})
			}

			section = check.Section
			rows = append(rows, []cell{{{check.Section, styleBold}}})
		}

		rows = append(rows, []cell{
			{{"  ", ""}, {statusText(check.Status), statusStyle(check.Status)}},
			{{check.Name, ""}},
			{{check.Detail, styleDim}},
		})
	}

	return writeTable(out, colors, rows)
}

func statusText(status health.Status) string {
	if status == health.StatusWarn || status == health.StatusFail {
		return fmt.Sprintf("%-4s", strings.ToUpper(string(status)))
	}

	return string(status)
}

func statusStyle(status health.Status) style {
	if status == health.StatusWarn || status == health.StatusFail {
		return styleBold + ";" + statusStyles[status]
	}

	return statusStyles[status]
}

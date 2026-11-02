package cli

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/monitor"
	"github.com/hugoh/tamis/internal/store"
	"github.com/spf13/cobra"
)

// day is how long a day is on the clock, which is all that matters here.
const day = 24 * time.Hour

// qualityEvery is how often the quality check is pinged.
const qualityEvery = day

// monitors report to the services configured in [monitor].
type monitors struct {
	beat    *monitor.Pinger
	quality *monitor.Pinger
	// next is when the quality check is due.
	next time.Time
}

// newMonitors starts the pingers for the checks that are configured; the
// others are nil and do nothing.
func newMonitors(ctx context.Context, cfg *config.Config, logger *slog.Logger) (*monitors, error) {
	beat, err := monitor.New(ctx, cfg.Monitor.PingURL, logger)
	if err != nil {
		return nil, fmt.Errorf("monitor.ping_url: %w", err)
	}

	quality, err := monitor.New(ctx, cfg.Monitor.QualityURL, logger)
	if err != nil {
		beat.Close()

		return nil, fmt.Errorf("monitor.quality_url: %w", err)
	}

	return &monitors{beat: beat, quality: quality}, nil
}

func (m *monitors) close() {
	m.beat.Close()
	m.quality.Close()
}

// checkQuality pings the quality check when it is due: a failure listing what
// looks wrong with the training or the decisions, or a success.
func (m *monitors) checkQuality(
	ctx context.Context,
	database *store.Store,
	cfg *config.Config,
	logger *slog.Logger,
) {
	if m.quality == nil || time.Now().Before(m.next) {
		return
	}

	m.next = time.Now().Add(qualityEvery)

	report, err := health.Collect(ctx, database, cfg)
	if err != nil {
		logger.DebugContext(ctx, "could not check the filter's quality", "err", err)

		return
	}

	problems := report.QualityProblems()
	if len(problems) == 0 {
		m.quality.Success("the training and the decisions look healthy")

		return
	}

	var body strings.Builder

	for _, check := range problems {
		fmt.Fprintf(
			&body,
			"%s %s: %s\n",
			strings.ToUpper(string(check.Status)),
			check.Name,
			check.Detail,
		)
	}

	m.quality.Fail(body.String())
}

func (a *app) monitorCommand() *cobra.Command {
	var asJSON bool

	cmd := &cobra.Command{
		Use:   "monitor",
		Short: "Show the checks to set up on a monitoring service such as healthchecks.io",
		Long: "Work out, from poll_interval and monitor.fail_after, the period and grace time of\n" +
			"the two checks `tamis run` pings when [monitor] is configured, and how soon each\n" +
			"kind of trouble would alert.",
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			cfg, err := config.Load(a.env.Getenv, a.configPath)
			if err != nil {
				return err //nolint:wrapcheck // config errors already name the file
			}

			settings := monitor.Recommend(monitor.Inputs{
				Poll:        cfg.Policy.PollInterval.Duration,
				FailAfter:   cfg.Monitor.FailAfter,
				PassTimeout: passTimeout,
				MaxRetry:    maxPassRetry,
			})

			if asJSON {
				encoder := json.NewEncoder(cmd.OutOrStdout())
				encoder.SetIndent("", "  ")

				return encoder.Encode(settings)
			}

			return writeMonitor(cmd.OutOrStdout(), cfg, settings)
		},
	}

	cmd.Flags().BoolVar(&asJSON, "json", false, "print the settings as JSON")
	cmd.AddCommand(a.monitorTestCommand())

	return cmd
}

func (a *app) monitorTestCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "test",
		Short: "Send one ping to each configured check, to see that the URLs work",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			cfg, err := config.Load(a.env.Getenv, a.configPath)
			if err != nil {
				return err //nolint:wrapcheck // config errors already name the file
			}

			out := cmd.OutOrStdout()

			if cfg.Monitor.PingURL == "" && cfg.Monitor.QualityURL == "" {
				_, _ = fmt.Fprintln(out, "No monitor.ping_url or monitor.quality_url configured.")

				return nil
			}

			var failed error

			for _, check := range []struct{ name, url string }{
				{"heartbeat", cfg.Monitor.PingURL}, {"quality", cfg.Monitor.QualityURL},
			} {
				if check.url == "" {
					continue
				}

				if err := a.pingOnce(cmd.Context(), check.url); err != nil {
					_, _ = fmt.Fprintf(out, "%s: FAILED: %v\n", check.name, err)
					failed = fmt.Errorf("%s: %w", check.name, err)

					continue
				}

				_, _ = fmt.Fprintf(out, "%s: ok\n", check.name)
			}

			return failed
		},
	}
}

func (a *app) pingOnce(ctx context.Context, url string) error {
	pinger, err := monitor.New(ctx, url, slog.New(slog.DiscardHandler))
	if err != nil {
		return err //nolint:wrapcheck // the error names the setting
	}

	defer pinger.Close()

	return pinger.Test(ctx, "tamis monitor test") //nolint:wrapcheck // the error says what failed
}

func writeMonitor(out io.Writer, cfg *config.Config, settings monitor.Settings) error {
	table := tabwriter.NewWriter(out, 0, 0, tablePadding, ' ', 0)

	_, _ = fmt.Fprintln(table, "Create two checks on the service:")
	_, _ = fmt.Fprintln(table, "\tCHECK\tPERIOD\tGRACE\tCONFIG KEY")
	writeCheck(table, settings.Heartbeat, "monitor.ping_url", cfg.Monitor.PingURL != "")
	writeCheck(table, settings.Quality, "monitor.quality_url", cfg.Monitor.QualityURL != "")
	_, _ = fmt.Fprintln(table)
	_, _ = fmt.Fprintln(table, "They would alert after:")

	for _, alert := range settings.Alerts {
		_, _ = fmt.Fprintf(table, "\t%s\t%s\n", spanText(alert.AfterSeconds), alert.When)
	}

	if err := table.Flush(); err != nil {
		return fmt.Errorf("write settings: %w", err)
	}

	_, _ = fmt.Fprintln(out, "\nOr create them through the healthchecks.io API:")

	for _, check := range []monitor.Check{settings.Heartbeat, settings.Quality} {
		payload, err := json.Marshal(apiCheck{
			Name: check.Name, Timeout: check.Timeout, Grace: check.Grace, Unique: []string{"name"},
		})
		if err != nil {
			return fmt.Errorf("write settings: %w", err)
		}

		_, _ = fmt.Fprintf(
			out,
			"  curl -s https://healthchecks.io/api/v3/checks/ -H \"X-Api-Key: $KEY\" -d '%s'\n",
			payload,
		)
	}

	return nil
}

// apiCheck is the healthchecks.io API's body for creating a check.
type apiCheck struct {
	Name    string   `json:"name"`
	Timeout int      `json:"timeout"`
	Grace   int      `json:"grace"`
	Unique  []string `json:"unique"`
}

func writeCheck(out io.Writer, check monitor.Check, key string, configured bool) {
	state := key
	if !configured {
		state += " (not set)"
	}

	_, _ = fmt.Fprintf(out, "\t%s\t%s\t%s\t%s\n",
		check.Name, spanText(check.Timeout), spanText(check.Grace), state)
}

// spanText writes seconds the way people say them: "2m", "11m", "1d".
func spanText(seconds int) string {
	span := time.Duration(seconds) * time.Second

	switch {
	case span == 0:
		return "at once"
	case span%day == 0:
		return fmt.Sprintf("%dd", span/day)
	case span%time.Hour == 0:
		return fmt.Sprintf("%dh", span/time.Hour)
	case span%time.Minute == 0:
		return fmt.Sprintf("%dm", span/time.Minute)
	default:
		return span.String()
	}
}

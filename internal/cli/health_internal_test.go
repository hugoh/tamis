package cli

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/retry"
)

var errDown = errors.New("connection refused")

func newHealth() (*passHealth, *bytes.Buffer, *slog.Logger) {
	out := new(bytes.Buffer)
	logger := slog.New(slog.NewTextHandler(out, &slog.HandlerOptions{Level: slog.LevelDebug}))

	return &passHealth{pace: retry.Backoff(time.Second, time.Minute)}, out, logger
}

func levelsOf(out string) []string {
	var levels []string

	for line := range strings.Lines(out) {
		_, rest, _ := strings.Cut(line, "level=")
		level, _, _ := strings.Cut(rest, " ")
		message := strings.TrimPrefix(strings.SplitN(rest, "msg=", 2)[1], `"`)
		levels = append(levels, level+" "+strings.Fields(message)[0])
	}

	return levels
}

func TestPassHealthLogsTheStartRemindersAndTheEnd(t *testing.T) {
	t.Parallel()

	health, out, logger := newHealth()

	for range 6 {
		require.Positive(t, health.after(t.Context(), logger, errDown))
	}

	require.Zero(t, health.after(t.Context(), logger, nil))
	require.Zero(t, health.after(t.Context(), logger, nil), "success is quiet once recovered")

	require.Equal(t, []string{
		"ERROR pass", // the first failure
		"WARN pass",  // 2nd: still failing
		"WARN pass",  // 3rd
		"DEBUG pass", // 4th: routine
		"WARN pass",  // 5th
		"DEBUG pass", // 6th
		"INFO pass",  // recovered
	}, levelsOf(out.String()))
	require.Contains(t, out.String(), "msg=\"pass recovered\" failures=6")
}

func TestPassHealthWaitsLongerAfterEachFailure(t *testing.T) {
	t.Parallel()

	health, _, logger := newHealth()

	first := health.after(t.Context(), logger, errDown)

	var last time.Duration
	for range 5 {
		last = health.after(t.Context(), logger, errDown)
	}

	require.Greater(t, last, 2*first)

	health.after(t.Context(), logger, nil)
	require.Less(t, health.after(t.Context(), logger, errDown), 2*first, "a success starts over")
}

func TestPassHealthReportsARejectedLoginOnceThenHourly(t *testing.T) {
	t.Parallel()

	health, out, logger := newHealth()
	rejected := fmt.Errorf("pass: %w", mail.ErrLogin)

	for range 8 {
		require.Equal(t, loginRetry, health.after(t.Context(), logger, rejected))
	}

	health.after(t.Context(), logger, nil)

	require.Equal(t, 1, strings.Count(out.String(), "level=ERROR"))
	require.Equal(t, 2, strings.Count(out.String(), "level=WARN"), "the 4th and 8th")
	require.Contains(t, out.String(), "msg=\"login accepted again\" failures=8")
}

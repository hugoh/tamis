package monitor

import (
	"fmt"
	"time"

	"github.com/hugoh/tamis/internal/retry"
)

const (
	day = 24 * time.Hour
	// qualityGrace lets the daily quality ping be late, as after a restart.
	qualityGrace = 2 * time.Hour
	// minPeriod is the shortest period healthchecks.io accepts.
	minPeriod = time.Minute
	// passSlack is added to the longest a pass may take, for the settle wait
	// and the network.
	passSlack = time.Minute
)

// Check is how to set up one check on the service. The field names are those
// of the healthchecks.io API, in seconds.
type Check struct {
	Name string `json:"name"`
	// Timeout is the period: how long the service waits for a ping.
	Timeout int `json:"timeout"`
	// Grace is how much longer it waits before alerting.
	Grace int `json:"grace"`
}

// Alert is how soon the service alerts for one kind of trouble.
type Alert struct {
	When         string `json:"when"`
	AfterSeconds int    `json:"afterSeconds"`
}

// Settings are the checks to create on the service, and when each kind of
// trouble would alert.
type Settings struct {
	Heartbeat Check   `json:"heartbeat"`
	Quality   Check   `json:"quality"`
	Alerts    []Alert `json:"alerts"`
}

// Inputs are what the settings are worked out from.
type Inputs struct {
	// Poll is policy.poll_interval: a healthy daemon passes at least this often.
	Poll time.Duration
	// FailAfter is monitor.fail_after.
	FailAfter int
	// PassTimeout is the longest a pass may take before it is cut short.
	PassTimeout time.Duration
	// MaxRetry is the longest wait after failed passes.
	MaxRetry time.Duration
}

// Recommend works out the checks' settings and when they would alert.
func Recommend(inputs Inputs) Settings {
	period := max(inputs.Poll.Round(time.Second), minPeriod)
	grace := inputs.PassTimeout + passSlack

	return Settings{
		Heartbeat: Check{"tamis heartbeat", seconds(period), seconds(grace)},
		Quality:   Check{"tamis quality", seconds(day), seconds(qualityGrace)},
		Alerts: []Alert{
			{"the daemon stops, hangs or its host goes down", seconds(period + grace)},
			{
				fmt.Sprintf("the mail server is unreachable (fail_after = %d)", inputs.FailAfter),
				seconds(failureWait(inputs)),
			},
			{"the login is rejected", 0},
			{"the filter's training or decisions look unhealthy", seconds(day)},
		},
	}
}

// failureWait is about how long the failAfter-th failed pass in a row comes
// after the first, adding the waits between them.
func failureWait(inputs Inputs) time.Duration {
	pace := retry.Backoff(inputs.Poll, inputs.MaxRetry)
	pace.RandomizationFactor = 0
	pace.Reset()

	var total time.Duration

	for range inputs.FailAfter - 1 {
		total += pace.NextBackOff()
	}

	return total
}

func seconds(span time.Duration) int {
	return int(span / time.Second)
}

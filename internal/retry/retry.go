// Package retry paces the daemon's reconnection attempts: quick at first, then
// slower and slower, so an outage is noticed promptly but not hammered.
package retry

import (
	"time"

	"github.com/cenkalti/backoff/v5"
)

const (
	multiplier = 2
	jitter     = 0.2
)

// Backoff returns a sequence of delays that starts near first, doubles on each
// call to NextBackOff up to about limit, and varies by 20% so that several
// connections don't retry in step. It never gives up; Reset starts it over
// after a success. The limit is raised to first when smaller.
func Backoff(first, limit time.Duration) *backoff.ExponentialBackOff {
	bo := backoff.NewExponentialBackOff()
	bo.InitialInterval = first
	bo.Multiplier = multiplier
	bo.RandomizationFactor = jitter
	bo.MaxInterval = max(first, limit)
	bo.Reset()

	return bo
}

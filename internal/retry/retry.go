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

// Streak counts consecutive failures of one thing, for logging the start of
// a trouble, a few reminders while it lasts, and its end. The zero value is
// ready to use; it is not safe for concurrent use.
type Streak struct {
	failures int
	since    time.Time
}

// Fail records a failure at now and returns how many have happened in a row.
func (s *Streak) Fail(now time.Time) int {
	if s.failures == 0 {
		s.since = now
	}

	s.failures++

	return s.failures
}

// Since is when the current streak began; it is zero when there is none.
func (s *Streak) Since() time.Time {
	if s.failures == 0 {
		return time.Time{}
	}

	return s.since
}

// Recover ends the streak at now. It reports how many failures there were and
// for how long, and ok is false when there was no streak to end.
func (s *Streak) Recover(now time.Time) (int, time.Duration, bool) {
	if s.failures == 0 {
		return 0, 0, false
	}

	failures, downtime := s.failures, now.Sub(s.since)
	*s = Streak{}

	return failures, downtime.Round(time.Second), true
}

// Notable says whether the nth failure in a row deserves a log line above
// debug: the first few, then ever more rarely, so that a long outage leaves a
// handful of lines.
func Notable(failures int) bool {
	switch failures {
	case 1, 2, 3, 5, 10:
		return true
	}

	return failures > 0 && failures%20 == 0
}

package health

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/live"
	"github.com/hugoh/tamis/internal/store"
)

// quietGrace is how long past the time it was due the daemon may stay silent
// before it is called gone: a pass may take up to ten minutes.
const quietGrace = 11 * time.Minute

// DaemonInfo is what `tamis run` last saved about itself. Saved is nil when
// it has never saved anything.
type DaemonInfo struct {
	Saved *live.Snapshot
	// Poll is policy.poll_interval: how often the daemon should report.
	Poll time.Duration
}

// LoadDaemon reads what the daemon saved in the state database. A record
// that can't be read counts as none.
func LoadDaemon(
	ctx context.Context,
	database *store.Store,
	poll time.Duration,
) (DaemonInfo, error) {
	info := DaemonInfo{Poll: poll}

	text, found, err := database.Setting(ctx, live.SettingKey)
	if err != nil {
		return info, err //nolint:wrapcheck // store errors carry their context
	}

	if !found {
		return info, nil
	}

	if snap, err := live.Decode(text); err == nil {
		info.Saved = &snap
	}

	return info, nil
}

// Describe judges how the daemon is doing and says so in a line.
func (info DaemonInfo) Describe(now time.Time) (Status, string) {
	check := daemon(info, now)

	return check.Status, check.Detail
}

func daemon(info DaemonInfo, now time.Time) Check {
	snap := info.Saved
	if snap == nil {
		return newCheck(NameDaemon, StatusSkip, "nothing saved yet; has 'tamis run' ever run?")
	}

	due := snap.Updated.Add(info.Poll)
	if snap.Failures > 0 && !snap.RetryAt.IsZero() {
		due = snap.RetryAt
	}

	switch {
	case now.After(due.Add(quietGrace)):
		return newCheck(NameDaemon, StatusWarn, fmt.Sprintf(
			"last heard from %s ago (%s); is 'tamis run' going?",
			elapsed(now.Sub(snap.Updated)), stamp(snap.Updated),
		))
	case snap.Rejected:
		return newCheck(NameDaemon, StatusFail, fmt.Sprintf(
			"the server rejects the login, since %s (%d tries); check the password, "+
				"next try %s",
			clock(snap.FailingSince), snap.Failures, clock(snap.RetryAt),
		))
	case snap.Failures > 0:
		return newCheck(NameDaemon, StatusWarn, fmt.Sprintf(
			"%d passes failed in a row since %s: %s; next try %s",
			snap.Failures, clock(snap.FailingSince), snap.LastError, clock(snap.RetryAt),
		))
	case snap.Passes == 0:
		return newCheck(NameDaemon, StatusOK, "running since "+clock(snap.Started)+", no pass yet")
	}

	var detail strings.Builder

	fmt.Fprintf(&detail, "last pass %s ago: %s", elapsed(now.Sub(snap.LastPass)), snap.LastSummary)

	if !snap.RecoveredAt.IsZero() {
		fmt.Fprintf(&detail, "; recovered %s ago from a failure at %s",
			elapsed(now.Sub(snap.RecoveredAt)), clock(snap.LastErrorAt))
	}

	return newCheck(NameDaemon, StatusOK, detail.String())
}

func stamp(at time.Time) string {
	return at.Local().Format(time.DateTime) //nolint:gosmopolitan // a local CLI shows local time
}

func clock(at time.Time) string {
	if at.IsZero() {
		return "?"
	}

	return at.Local().Format(time.TimeOnly) //nolint:gosmopolitan // a local CLI shows local time
}

func elapsed(span time.Duration) time.Duration {
	if span < time.Minute {
		return span.Round(time.Second)
	}

	return span.Round(time.Minute)
}

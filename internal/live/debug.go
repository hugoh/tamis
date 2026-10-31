package live

import (
	"log/slog"
	"sync"
	"time"
)

// DebugSwitch lets debug logging be turned on for a while the daemon
// runs, and turns itself off again, so it can't be left on by accident. Its
// Level goes to the log handlers, which ask it on every record. A nil switch
// does nothing.
type DebugSwitch struct {
	mu    sync.Mutex
	level slog.LevelVar
	base  slog.Level
	span  time.Duration
	timer *time.Timer
	until time.Time
}

// NewDebugSwitch starts at base and, when turned on, stays on for span.
func NewDebugSwitch(base slog.Level, span time.Duration) *DebugSwitch {
	debug := &DebugSwitch{base: base, span: span}
	debug.level.Set(base)

	return debug
}

// Level is what the log handlers consult.
func (d *DebugSwitch) Level() slog.Leveler {
	return &d.level
}

// Span is how long turning debug on lasts.
func (d *DebugSwitch) Span() time.Duration {
	if d == nil {
		return 0
	}

	return d.span
}

// Locked reports whether debug is already on for good, as under --verbose,
// which the switch can't change.
func (d *DebugSwitch) Locked() bool {
	return d != nil && d.base <= slog.LevelDebug
}

// On turns debug logging on for the span; turning it on again restarts it.
func (d *DebugSwitch) On() {
	if d == nil || d.Locked() {
		return
	}

	d.mu.Lock()
	defer d.mu.Unlock()

	if d.timer != nil {
		d.timer.Stop()
	}

	d.level.Set(slog.LevelDebug)
	d.until = time.Now().Add(d.span)
	d.timer = time.AfterFunc(d.span, d.Off)
}

// Off returns to the starting level.
func (d *DebugSwitch) Off() {
	if d == nil || d.Locked() {
		return
	}

	d.mu.Lock()
	defer d.mu.Unlock()

	if d.timer != nil {
		d.timer.Stop()
		d.timer = nil
	}

	d.level.Set(d.base)
	d.until = time.Time{}
}

// State says whether debug was turned on, and until when.
func (d *DebugSwitch) State() (bool, time.Time) {
	if d == nil {
		return false, time.Time{}
	}

	d.mu.Lock()
	defer d.mu.Unlock()

	return !d.until.IsZero(), d.until
}

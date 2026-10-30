// Package live holds what the running daemon knows about itself right now,
// for the web interface to show: what the last pass did, what failed, and
// the recent log lines.
package live

import (
	"sync"
	"time"
)

// What the daemon is doing: waiting for new mail (Idle), waiting out the poll
// interval (Polling), or judging mail (Running).
const (
	Idle    = "idle"
	Polling = "polling"
	Running = "running"
)

// Snapshot is a copy of the status at one moment.
type Snapshot struct {
	Started     time.Time
	Passes      int
	LastPass    time.Time
	LastSummary string
	LastError   string
	LastErrorAt time.Time
	// Waiting is Idle, Polling or Running; empty before the first pass starts.
	Waiting string
}

// Status is the daemon's live state. A nil *Status ignores updates and
// reports an empty Snapshot.
type Status struct {
	mu   sync.Mutex
	snap Snapshot
}

// NewStatus starts a status for a daemon that started at the given time.
func NewStatus(started time.Time) *Status {
	return &Status{snap: Snapshot{Started: started}}
}

// PassDone records a successful pass.
func (s *Status) PassDone(at time.Time, summary string) {
	s.update(func(snap *Snapshot) {
		snap.Passes++
		snap.LastPass = at
		snap.LastSummary = summary
	})
}

// Failed records a failure of a pass or of waiting for the next one.
func (s *Status) Failed(at time.Time, err error) {
	s.update(func(snap *Snapshot) {
		snap.LastError = err.Error()
		snap.LastErrorAt = at
	})
}

// Wait records what the daemon is doing now: Idle, Polling or Running.
func (s *Status) Wait(mode string) {
	s.update(func(snap *Snapshot) { snap.Waiting = mode })
}

// Snapshot returns a copy of the current state.
func (s *Status) Snapshot() Snapshot {
	if s == nil {
		return Snapshot{}
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	return s.snap
}

func (s *Status) update(change func(*Snapshot)) {
	if s == nil {
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	change(&s.snap)
}

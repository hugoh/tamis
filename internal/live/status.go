// Package live holds what the running daemon knows about itself right now,
// for the web interface to show: what the last pass did, what failed, and
// the recent log lines.
package live

import (
	"encoding/json"
	"fmt"
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
	Started     time.Time `json:"started"`
	Passes      int       `json:"passes"`
	LastPass    time.Time `json:"last_pass"`
	LastSummary string    `json:"last_summary"`
	LastError   string    `json:"last_error"`
	LastErrorAt time.Time `json:"last_error_at"`
	// Failures counts the passes that have failed in a row, and RetryAt is
	// when the next try is due; both are cleared by a pass that succeeds.
	Failures     int       `json:"failures"`
	FailingSince time.Time `json:"failing_since"`
	RetryAt      time.Time `json:"retry_at"`
	// Rejected means the failures began with a rejected login.
	Rejected bool `json:"rejected"`
	// RecoveredAt is when a pass last succeeded after failures.
	RecoveredAt time.Time `json:"recovered_at"`
	// Updated is when the daemon last saved this, so that a reader can tell
	// a daemon that has stopped from one that is merely quiet.
	Updated time.Time `json:"updated"`
	// Waiting is Idle, Polling or Running; empty before the first pass starts.
	Waiting string `json:"waiting"`
}

// SettingKey is where the daemon saves its Snapshot in the state database,
// for the commands that run in another process to read.
const SettingKey = "daemon_status"

// Encode is the form a Snapshot is saved in.
func (s Snapshot) Encode() string {
	text, err := json.Marshal(s)
	if err != nil {
		return "{}" // a Snapshot of plain fields always marshals
	}

	return string(text)
}

// Decode reads what Encode wrote.
func Decode(text string) (Snapshot, error) {
	var snap Snapshot
	if err := json.Unmarshal([]byte(text), &snap); err != nil {
		return Snapshot{}, fmt.Errorf("daemon status: %w", err)
	}

	return snap, nil
}

// Status is the daemon's live state. A nil *Status ignores updates and
// reports an empty Snapshot.
type Status struct {
	mu      sync.Mutex
	snap    Snapshot
	persist func(Snapshot)
}

// NewStatus starts a status for a daemon that started at the given time.
func NewStatus(started time.Time) *Status {
	return &Status{snap: Snapshot{Started: started}}
}

// Persist has save called with the new Snapshot each time a pass ends or
// fails, and on Save, so that other processes can see how the daemon is.
// save is called without the status locked.
func (s *Status) Persist(save func(Snapshot)) {
	if s == nil {
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	s.persist = save
}

// Save passes the current state to the function given to Persist.
func (s *Status) Save(at time.Time) {
	s.update(func(snap *Snapshot) { snap.Updated = at }, true)
}

// PassDone records a successful pass.
func (s *Status) PassDone(at time.Time, summary string) {
	s.update(func(snap *Snapshot) {
		snap.Passes++
		snap.LastPass = at
		snap.LastSummary = summary

		if snap.Failures > 0 {
			snap.RecoveredAt = at
		}

		snap.Failures = 0
		snap.FailingSince = time.Time{}
		snap.RetryAt = time.Time{}
		snap.Rejected = false
		snap.Updated = at
	}, true)
}

// Failed records a failure of a pass or of waiting for the next one.
func (s *Status) Failed(at time.Time, err error) {
	s.update(func(snap *Snapshot) {
		snap.LastError = err.Error()
		snap.LastErrorAt = at

		if snap.Failures == 0 {
			snap.FailingSince = at
			snap.Rejected = false
		}

		snap.Failures++
		snap.Updated = at
	}, true)
}

// RetryAt records when the next try after a failure is due.
func (s *Status) RetryAt(at time.Time) {
	s.update(func(snap *Snapshot) { snap.RetryAt = at }, true)
}

// LoginRejected records that the failure was a rejected login.
func (s *Status) LoginRejected() {
	s.update(func(snap *Snapshot) { snap.Rejected = true }, true)
}

// Wait records what the daemon is doing now: Idle, Polling or Running.
func (s *Status) Wait(mode string) {
	s.update(func(snap *Snapshot) { snap.Waiting = mode }, false)
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

func (s *Status) update(change func(*Snapshot), save bool) {
	if s == nil {
		return
	}

	s.mu.Lock()
	change(&s.snap)
	snap, persist := s.snap, s.persist
	s.mu.Unlock()

	if save && persist != nil {
		persist(snap)
	}
}

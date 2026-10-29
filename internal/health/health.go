// Package health judges whether the filter is in good shape from what the
// store holds: how balanced its training is, how it has been deciding and
// whether it is still deciding at all.
package health

import (
	"encoding/json"
	"fmt"
	"io"
	"slices"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/store"
)

// Status is the outcome of a single check.
type Status string

// Check outcomes.
const (
	StatusOK   Status = "ok"
	StatusWarn Status = "warn"
	StatusFail Status = "fail"
	StatusSkip Status = "skip"
)

// Window is how many recent decisions the verdict checks look at.
const Window = 100

// Check names.
const (
	NameBalance     = "class balance"
	NameUncertain   = "uncertain rate"
	NameCorrections = "correction rate"
	NameLast        = "last decision"
	NameJournal     = "journal"
	NameRecent      = "recent verdicts"
)

const (
	percent        = 100
	minPerClass    = 50
	maxImbalance   = 10
	minSample      = 20
	maxUncertain   = 0.3
	maxCorrections = 0.1
	staleAfter     = 24 * time.Hour
	columnPadding  = 2
)

// Check is the result of one health check.
type Check struct {
	Name   string `json:"name"`
	Status Status `json:"status"`
	Detail string `json:"detail,omitempty"`
}

// Report collects the checks.
type Report struct {
	Checks []Check `json:"checks"`
}

// Input is what the checks look at. Recent is newest first.
type Input struct {
	Stats  store.Stats
	Recent []store.Decision
	// Retention is how long the journal is kept; zero means forever.
	Retention time.Duration
	Now       time.Time
}

// Failed reports whether any check failed.
func (r *Report) Failed() bool {
	return slices.ContainsFunc(
		r.Checks,
		func(check Check) bool { return check.Status == StatusFail },
	)
}

// Run evaluates every check.
func Run(input Input) *Report {
	judged := judgements(input.Recent)

	return &Report{Checks: []Check{
		balance(input.Stats),
		uncertainRate(judged),
		correctionRate(input.Recent, len(judged)),
		lastDecision(input.Recent, input.Now),
		journal(input.Stats, input.Retention),
		recentVerdicts(input.Recent),
	}}
}

func balance(stats store.Stats) Check {
	detail := fmt.Sprintf("%d spam, %d good", stats.Spam, stats.Good)
	fewest, most := min(stats.Spam, stats.Good), max(stats.Spam, stats.Good)

	switch {
	case fewest == 0:
		return Check{NameBalance, StatusFail, detail + ": it needs examples of both"}
	case fewest < minPerClass:
		return Check{NameBalance, StatusWarn, detail + ": fewer than 50 of one kind"}
	case most > maxImbalance*fewest:
		return Check{NameBalance, StatusWarn, detail + ": more than 10 to 1"}
	default:
		return Check{NameBalance, StatusOK, detail}
	}
}

// judgements keeps the decisions tamis made on its own.
func judgements(recent []store.Decision) []store.Decision {
	return slices.DeleteFunc(slices.Clone(recent), func(d store.Decision) bool {
		return d.Reason != engine.ReasonBayes && d.Reason != engine.ReasonWhitelisted
	})
}

func uncertainRate(judged []store.Decision) Check {
	const name = NameUncertain

	if len(judged) < minSample {
		return tooFew(name, len(judged))
	}

	uncertain := 0

	for _, d := range judged {
		if d.Verdict == engine.VerdictUncertain {
			uncertain++
		}
	}

	return rate(name, uncertain, len(judged), maxUncertain, "uncertain")
}

func correctionRate(recent []store.Decision, judged int) Check {
	const name = NameCorrections

	if judged < minSample {
		return tooFew(name, judged)
	}

	corrections := 0

	for _, d := range recent {
		if d.Reason == engine.ReasonCorrection || d.Reason == engine.ReasonUndo {
			corrections++
		}
	}

	return rate(name, corrections, judged, maxCorrections, "corrected")
}

func tooFew(name string, judged int) Check {
	return Check{name, StatusSkip, fmt.Sprintf("%d judged decisions, need %d", judged, minSample)}
}

func rate(name string, count, total int, limit float64, what string) Check {
	share := float64(count) / float64(total)
	detail := fmt.Sprintf(
		"%d of the last %d judged (%.0f%%) %s",
		count,
		total,
		share*percent,
		what,
	)

	if share > limit {
		return Check{name, StatusWarn, detail}
	}

	return Check{name, StatusOK, detail}
}

func lastDecision(recent []store.Decision, now time.Time) Check {
	const name = NameLast

	if len(recent) == 0 {
		return Check{name, StatusWarn, "none yet; has 'tamis run' ever run?"}
	}

	age := now.Sub(recent[0].At).Round(time.Minute)
	if age > staleAfter {
		return Check{name, StatusWarn, fmt.Sprintf("%s ago; is 'tamis run' running?", age)}
	}

	return Check{name, StatusOK, fmt.Sprintf("%s ago", age)}
}

func journal(stats store.Stats, retention time.Duration) Check {
	if stats.Decisions == 0 {
		return Check{NameJournal, StatusSkip, "empty"}
	}

	kept := "kept forever"
	if retention > 0 {
		kept = fmt.Sprintf("kept %d days", retention/(24*time.Hour)) //nolint:mnd // hours per day
	}

	return Check{NameJournal, StatusOK, fmt.Sprintf("%d decisions since %s, %s",
		stats.Decisions, stats.OldestDecision.Format(time.DateOnly), kept)}
}

func recentVerdicts(recent []store.Decision) Check {
	if len(recent) == 0 {
		return Check{NameRecent, StatusSkip, "no decisions yet"}
	}

	verdicts := map[string]int{}
	corrections := 0

	for _, decision := range judgements(recent) {
		verdicts[decision.Verdict]++
	}

	for _, decision := range recent {
		if decision.Reason == engine.ReasonCorrection || decision.Reason == engine.ReasonUndo {
			corrections++
		}
	}

	return Check{NameRecent, StatusOK, fmt.Sprintf(
		"last %d: %d good, %d spam, %d uncertain; %d correction",
		len(recent), verdicts[engine.VerdictGood], verdicts[engine.VerdictSpam],
		verdicts[engine.VerdictUncertain], corrections,
	)}
}

// WriteText renders the report for a terminal, with problems in capitals.
func (r *Report) WriteText(w io.Writer) error {
	table := tabwriter.NewWriter(w, 0, 0, columnPadding, ' ', 0)

	for _, check := range r.Checks {
		status := string(check.Status)
		if check.Status == StatusWarn || check.Status == StatusFail {
			status = strings.ToUpper(status)
		}

		_, _ = fmt.Fprintf(table, "%s\t%s\t%s\n", status, check.Name, check.Detail)
	}

	if err := table.Flush(); err != nil {
		return fmt.Errorf("write report: %w", err)
	}

	return nil
}

// WriteJSON renders the report as indented JSON.
func (r *Report) WriteJSON(w io.Writer) error {
	enc := json.NewEncoder(w)
	enc.SetIndent("", "  ")

	if err := enc.Encode(r); err != nil {
		return fmt.Errorf("write report: %w", err)
	}

	return nil
}

// Package health judges whether the filter is in good shape from what the
// store holds: how balanced its training is, how it has been deciding and
// whether it is still deciding at all.
package health

import (
	"fmt"
	"io"
	"slices"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/report"
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
	NameAuthID      = "provider auth id"
	NameWeb         = "web interface"
	NamePasskeys    = "web passkeys"
	NameConfig      = "config file"
	NameRules       = "filing rules"
)

// Sections group the checks in a report, in this order.
const (
	SectionConfig    = "Configuration"
	SectionRules     = "Rules"
	SectionTraining  = "Training"
	SectionDecisions = "Decisions"
	SectionWeb       = "Web interface"
)

var sections = map[string]string{ //nolint:gochecknoglobals // constant lookup
	NameConfig:      SectionConfig,
	NameAuthID:      SectionConfig,
	NameRules:       SectionRules,
	NameBalance:     SectionTraining,
	NameUncertain:   SectionDecisions,
	NameCorrections: SectionDecisions,
	NameLast:        SectionDecisions,
	NameJournal:     SectionDecisions,
	NameRecent:      SectionDecisions,
	NameWeb:         SectionWeb,
	NamePasskeys:    SectionWeb,
}

const (
	percent        = 100
	minPerClass    = 50
	maxImbalance   = 10
	minSample      = 20
	maxUncertain   = 0.3
	maxCorrections = 0.1
	staleAfter     = 24 * time.Hour
)

// Check is the result of one health check.
type Check struct {
	Section string `json:"section"`
	Name    string `json:"name"`
	Status  Status `json:"status"`
	Detail  string `json:"detail,omitempty"`
}

func newCheck(name string, status Status, detail string) Check {
	return Check{Section: sections[name], Name: name, Status: status, Detail: detail}
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
	Auth      engine.AuthPin
	Web       Web
	Config    ConfigInfo
	Rules     RulesInfo
	Now       time.Time
}

// ConfigInfo describes the configuration that loaded.
type ConfigInfo struct {
	Path, Account, Mode string
}

// RulesInfo is what compiling the filing rules found. Script is empty when
// none are configured.
type RulesInfo struct {
	Script  string
	Targets []string
	Err     error
}

// Web is what the checks know about the web interface.
type Web struct {
	Enabled bool
	Listen  string
	Origin  string
	// Reachable is whether something accepted a connection on Listen: that
	// is `tamis run`, if it is going.
	Reachable bool
	Passkeys  int
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

	checks := []Check{
		configFile(input.Config),
		authID(input.Auth),
		filingRules(input.Rules),
		balance(input.Stats),
		uncertainRate(judged),
		correctionRate(judged),
		lastDecision(input.Recent, input.Now),
		journal(input.Stats, input.Retention),
		recentVerdicts(input.Recent, judged),
		webInterface(input.Web),
	}

	if input.Web.Enabled {
		checks = append(checks, passkeys(input.Web.Passkeys))
	}

	return &Report{Checks: checks}
}

// ConfigFailed is the report for a configuration that doesn't load. When the
// problem is in the filing rules, it is shown under Rules.
func ConfigFailed(err error, inRules bool) *Report {
	name := NameConfig
	if inRules {
		name = NameRules
	}

	return &Report{Checks: []Check{newCheck(name, StatusFail, err.Error())}}
}

func configFile(info ConfigInfo) Check {
	return newCheck(NameConfig, StatusOK,
		fmt.Sprintf("%s: %s, %s mode", info.Path, info.Account, info.Mode))
}

func filingRules(rules RulesInfo) Check {
	switch {
	case rules.Script == "":
		return newCheck(NameRules, StatusSkip, "not configured")
	case rules.Err != nil:
		return newCheck(NameRules, StatusFail, fmt.Sprintf("%s: %v", rules.Script, rules.Err))
	case len(rules.Targets) == 0:
		return newCheck(NameRules, StatusWarn,
			rules.Script+": no fileinto commands, so nothing is filed")
	}

	noun := "folders"
	if len(rules.Targets) == 1 {
		noun = "folder"
	}

	return newCheck(NameRules, StatusOK, fmt.Sprintf("%s: files into %d %s: %s",
		rules.Script, len(rules.Targets), noun, strings.Join(rules.Targets, ", ")))
}

func webInterface(web Web) Check {
	switch {
	case !web.Enabled:
		return newCheck(NameWeb, StatusSkip, "not enabled")
	case !web.Reachable:
		return newCheck(NameWeb, StatusWarn, fmt.Sprintf(
			"nothing is listening on %s; is 'tamis run' going?", web.Listen,
		))
	default:
		return newCheck(
			NameWeb,
			StatusOK,
			fmt.Sprintf("listening on %s, at %s", web.Listen, web.Origin),
		)
	}
}

func passkeys(count int) Check {
	if count == 0 {
		return newCheck(NamePasskeys, StatusWarn, "none enrolled; run 'tamis web enroll'")
	}

	return newCheck(NamePasskeys, StatusOK, fmt.Sprintf("%d enrolled", count))
}

func balance(stats store.Stats) Check {
	detail := fmt.Sprintf("%d spam, %d good", stats.Spam, stats.Good)
	fewest, most := min(stats.Spam, stats.Good), max(stats.Spam, stats.Good)

	switch {
	case fewest == 0:
		return newCheck(NameBalance, StatusFail, detail+": it needs examples of both")
	case fewest < minPerClass:
		return newCheck(NameBalance, StatusWarn, detail+": fewer than 50 of one kind")
	case most > maxImbalance*fewest:
		return newCheck(NameBalance, StatusWarn, detail+": more than 10 to 1")
	default:
		return newCheck(NameBalance, StatusOK, detail)
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

func correctionRate(judged []store.Decision) Check {
	const name = NameCorrections

	if len(judged) < minSample {
		return tooFew(name, len(judged))
	}

	return rate(name, correctedCount(judged), len(judged), maxCorrections, "corrected")
}

// correctedCount is how many of the decisions were later corrected.
func correctedCount(decisions []store.Decision) int {
	corrected := 0

	for _, decision := range decisions {
		if decision.CorrectedBy > 0 {
			corrected++
		}
	}

	return corrected
}

func tooFew(name string, judged int) Check {
	return newCheck(
		name,
		StatusSkip,
		fmt.Sprintf("%d judged decisions, need %d", judged, minSample),
	)
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
		return newCheck(name, StatusWarn, detail)
	}

	return newCheck(name, StatusOK, detail)
}

func lastDecision(recent []store.Decision, now time.Time) Check {
	const name = NameLast

	if len(recent) == 0 {
		return newCheck(name, StatusWarn, "none yet; has 'tamis run' ever run?")
	}

	age := now.Sub(recent[0].At).Round(time.Minute)
	if age > staleAfter {
		return newCheck(name, StatusWarn, fmt.Sprintf("%s ago; is 'tamis run' running?", age))
	}

	return newCheck(name, StatusOK, fmt.Sprintf("%s ago", age))
}

func authID(pin engine.AuthPin) Check {
	switch pin.Source {
	case engine.AuthStale:
		return newCheck(NameAuthID, StatusWarn, fmt.Sprintf(
			"learnt for %s but the account now uses %s, so it is not used; run 'tamis bootstrap'",
			pin.LearntFor, pin.Host,
		))
	case engine.AuthUnset:
		return newCheck(
			NameAuthID, StatusSkip,
			"not learnt; only the topmost header counts (run 'tamis bootstrap' to learn it)",
		)
	case engine.AuthOff:
		return newCheck(NameAuthID, StatusOK, "off: only the topmost header counts")
	case engine.AuthConfigured, engine.AuthLearnt:
		return newCheck(NameAuthID, StatusOK, fmt.Sprintf("%s (%s)", pin.ID, pin.Source))
	}

	return newCheck(NameAuthID, StatusSkip, "unknown")
}

func journal(stats store.Stats, retention time.Duration) Check {
	if stats.Decisions == 0 {
		return newCheck(NameJournal, StatusSkip, "empty")
	}

	kept := "kept forever"
	if retention > 0 {
		kept = fmt.Sprintf("kept %d days", retention/(24*time.Hour)) //nolint:mnd // hours per day
	}

	return newCheck(NameJournal, StatusOK, fmt.Sprintf("%d decisions since %s, %s",
		stats.Decisions, stats.OldestDecision.Format(time.DateOnly), kept))
}

func recentVerdicts(recent, judged []store.Decision) Check {
	if len(recent) == 0 {
		return newCheck(NameRecent, StatusSkip, "no decisions yet")
	}

	verdicts := map[string]int{}

	for _, decision := range judged {
		verdicts[decision.Verdict]++
	}

	return newCheck(NameRecent, StatusOK, fmt.Sprintf(
		"last %d: %d good, %d spam, %d uncertain; %d corrected",
		len(recent), verdicts[engine.VerdictGood], verdicts[engine.VerdictSpam],
		verdicts[engine.VerdictUncertain], correctedCount(judged),
	))
}

// WriteJSON renders the report as indented JSON.
func (r *Report) WriteJSON(w io.Writer) error {
	return report.WriteJSON(w, r) //nolint:wrapcheck // the error already says "write report"
}

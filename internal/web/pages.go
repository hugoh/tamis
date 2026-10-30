package web

import (
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"time"

	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/health"
	"github.com/hugoh/tamis/internal/store"
)

const (
	pageDecisions = "decisions"
	pageHealth    = "health"
	pageWhitelist = "whitelist"
	pageLogs      = "logs"

	defaultDecisions = 100
	maxDecisions     = 500
	timeLayout       = "2006-01-02 15:04"
)

// view is what every page template receives.
type view struct {
	Title string
	Nav   string
	Body  any
}

type decisionRow struct {
	ID                           int64
	When, Verdict, Reason, Score string
	Action, From, Subject        string
	CanUndo, CanLearn            bool
	// Note is the outcome of an action just taken on this row.
	Note   string
	Failed bool
}

type filter struct {
	Label, Href string
	Active      bool
}

type decisionsBody struct {
	Rows    []decisionRow
	Verdict string
	Filters []filter
}

type checkRow struct {
	Status, Name, Detail string
}

type addressRow struct {
	Address, Source, Added string
}

func verdicts() []string {
	return []string{engine.VerdictSpam, engine.VerdictUncertain, engine.VerdictGood}
}

func (s *Server) render(resp http.ResponseWriter, name string, data view) {
	resp.Header().Set("Content-Type", "text/html; charset=utf-8")

	if err := s.pages[name].ExecuteTemplate(resp, "layout", data); err != nil {
		s.log.Error("render page", "page", name, "err", err)
	}
}

func localClock(when time.Time) string {
	local := when.Local() //nolint:gosmopolitan // the daemon runs where its user lives

	return local.Format(time.TimeOnly)
}

func localTime(when time.Time) string {
	local := when.Local() //nolint:gosmopolitan // the daemon runs where its user lives

	return local.Format(timeLayout)
}

// decisionQuery reads the limit and verdict filter, ignoring bad values.
func decisionQuery(query url.Values) (int, string) {
	limit, err := strconv.Atoi(query.Get("n"))
	if err != nil || limit < 1 || limit > maxDecisions {
		limit = defaultDecisions
	}

	verdict := query.Get("verdict")
	if !slices.Contains(verdicts(), verdict) {
		verdict = ""
	}

	return limit, verdict
}

func (s *Server) decisions(resp http.ResponseWriter, req *http.Request) {
	limit, verdict := decisionQuery(req.URL.Query())

	// ponytail: the filter applies to the newest n decisions, not the whole
	// journal; push it into the store query if that's not enough.
	all, err := s.db.Decisions(req.Context(), limit)
	if err != nil {
		s.fail(resp, "list decisions", err)

		return
	}

	body := decisionsBody{
		Verdict: verdict,
		Filters: filters(verdict),
		Rows:    decisionRows(all, verdict),
	}

	if req.Header.Get("Hx-Request") == "true" {
		if err := s.pages[pageDecisions].ExecuteTemplate(resp, "table", body); err != nil {
			s.log.Error("render table", "err", err)
		}

		return
	}

	s.render(resp, pageDecisions, view{Title: pageDecisions, Nav: pageDecisions, Body: body})
}

func decisionRowFor(decision store.Decision) decisionRow {
	return decisionRow{
		ID:       decision.ID,
		When:     localTime(decision.At),
		Verdict:  decision.Verdict,
		Reason:   decision.Reason,
		Score:    strconv.FormatFloat(decision.Score, 'f', 2, 64),
		Action:   actionLabel(decision),
		From:     decision.From,
		Subject:  decision.Subject,
		CanUndo:  undoable(decision),
		CanLearn: learnable(decision),
	}
}

func decisionRows(all []store.Decision, verdict string) []decisionRow {
	var rows []decisionRow

	for _, decision := range all {
		if verdict != "" && decision.Verdict != verdict {
			continue
		}

		rows = append(rows, decisionRowFor(decision))
	}

	return rows
}

func filters(active string) []filter {
	list := make([]filter, 0, 1+len(verdicts()))
	list = append(list, filter{Label: "All", Href: "/", Active: active == ""})

	for _, verdict := range verdicts() {
		list = append(list, filter{
			Label:  verdict,
			Href:   "/?" + url.Values{"verdict": {verdict}}.Encode(),
			Active: verdict == active,
		})
	}

	return list
}

// actionLabel says what was done; what live mode would do is prefixed, and a
// later undo or correction is appended.
func actionLabel(decision store.Decision) string {
	label := decision.Action

	if decision.Action != "none" && !decision.Applied {
		label = "would " + label
	}

	switch {
	case decision.Undone:
		label += " (undone)"
	case decision.CorrectedBy > 0:
		label += " (corrected by " + strconv.FormatInt(decision.CorrectedBy, 10) + ")"
	}

	return label
}

type daemonView struct {
	Started, LastPass, Summary, Doing string
	Passes                            int
	LastError                         string
}

type healthBody struct {
	Checks []checkRow
	Daemon *daemonView
}

func (s *Server) health(resp http.ResponseWriter, req *http.Request) {
	report, err := health.Collect(req.Context(), s.db, s.cfg)
	if err != nil {
		s.fail(resp, "check health", err)

		return
	}

	body := healthBody{Checks: make([]checkRow, 0, len(report.Checks)), Daemon: s.daemon()}
	for _, check := range report.Checks {
		body.Checks = append(
			body.Checks,
			checkRow{Status: string(check.Status), Name: check.Name, Detail: check.Detail},
		)
	}

	s.render(resp, pageHealth, view{Title: pageHealth, Nav: pageHealth, Body: body})
}

// daemon describes the running daemon, or is nil when the web interface has
// no status to show.
func (s *Server) daemon() *daemonView {
	if s.status == nil {
		return nil
	}

	snap := s.status.Snapshot()
	daemon := &daemonView{
		Started: localTime(snap.Started),
		Passes:  snap.Passes,
		Doing:   snap.Waiting,
		Summary: snap.LastSummary,
	}

	if snap.LastPass.IsZero() {
		daemon.LastPass = "no pass yet"
	} else {
		daemon.LastPass = localTime(snap.LastPass)
	}

	if snap.LastError != "" {
		daemon.LastError = localTime(snap.LastErrorAt) + ": " + snap.LastError
	}

	return daemon
}

type logRow struct {
	When, Level, Text string
}

func (s *Server) logLines(resp http.ResponseWriter, req *http.Request) {
	entries := s.logs.Entries()
	rows := make([]logRow, 0, len(entries))

	for _, entry := range entries {
		rows = append(rows, logRow{
			When: localClock(entry.Time),

			Level: entry.Level.String(),
			Text:  entry.Text,
		})
	}

	body := struct{ Rows []logRow }{rows}

	if req.Header.Get("Hx-Request") == "true" {
		if err := s.pages[pageLogs].ExecuteTemplate(resp, "entries", body); err != nil {
			s.log.Error("render logs", "err", err)
		}

		return
	}

	s.render(resp, pageLogs, view{Title: pageLogs, Nav: pageLogs, Body: body})
}

type whitelistBody struct {
	Addresses []addressRow
	Error     string
}

func (s *Server) whitelist(resp http.ResponseWriter, req *http.Request) {
	body, err := s.whitelistBody(req, nil)
	if err != nil {
		s.fail(resp, "list whitelist", err)

		return
	}

	s.render(resp, pageWhitelist, view{Title: pageWhitelist, Nav: pageWhitelist, Body: body})
}

// renderAddresses answers an edit with the refreshed list, and what went
// wrong, if anything.
func (s *Server) renderAddresses(resp http.ResponseWriter, req *http.Request, editErr error) {
	body, err := s.whitelistBody(req, editErr)
	if err != nil {
		s.fail(resp, "list whitelist", err)

		return
	}

	resp.Header().Set("Content-Type", "text/html; charset=utf-8")

	if err := s.pages[pageWhitelist].ExecuteTemplate(resp, "addresses", body); err != nil {
		s.log.Error("render whitelist", "err", err)
	}
}

func (s *Server) whitelistBody(req *http.Request, editErr error) (whitelistBody, error) {
	addresses, err := s.db.Addresses(req.Context())
	if err != nil {
		return whitelistBody{}, err //nolint:wrapcheck // store errors carry their context
	}

	body := whitelistBody{Addresses: make([]addressRow, 0, len(addresses))}
	if editErr != nil {
		body.Error = editErr.Error()
	}

	for _, address := range addresses {
		body.Addresses = append(body.Addresses, addressRow{
			Address: address.Address, Source: address.Source, Added: localTime(address.AddedAt),
		})
	}

	return body, nil
}

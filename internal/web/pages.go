package web

import (
	"net/http"
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

	timeLayout = "2006-01-02 15:04"
)

// view is what every page template receives.
type view struct {
	Title   string
	Nav     string
	Body    any
	Version string
}

type decisionRow struct {
	ID                           int64
	When, Verdict, Reason, Score string
	Action, From, Subject        string
	CanUndo, CanLearn            bool
	// Undone is set once the action was reversed; CorrectedBy is the ID of the
	// later entry that corrected it, or 0.
	Undone      bool
	CorrectedBy int64
	// Note is the outcome of an action just taken on this row.
	Note   string
	Failed bool
}

type verdictChoice struct {
	Value, Label string
	Checked      bool
}

type decisionsBody struct {
	tableState

	Rows     []decisionRow
	Count    string
	Verdict  string
	Verdicts []verdictChoice
	Columns  []column
	MoreHref string
	// OnlyID is the one decision being shown, or 0.
	OnlyID int64
}

type checkRow struct {
	Status, Name, Detail string
}

type addressRow struct {
	Address, Source, Added string
}

func decisionSorts() []sortChoice {
	return []sortChoice{
		{store.SortTime, "Time", false},
		{store.SortID, "ID", false},
		{store.SortVerdict, "Verdict", true},
		{store.SortReason, "Reason", true},
		{store.SortScore, "Score", false},
		{store.SortAction, "Action", true},
		{store.SortFrom, "From", true},
		{store.SortSubject, "Subject", true},
	}
}

func verdicts() []string {
	return []string{engine.VerdictSpam, engine.VerdictUncertain, engine.VerdictGood}
}

func (s *Server) render(resp http.ResponseWriter, name string, data view) {
	resp.Header().Set("Content-Type", "text/html; charset=utf-8")

	data.Version = s.version

	if err := s.pages[name].ExecuteTemplate(resp, "layout", data); err != nil {
		s.log.Error("render page", "page", name, "err", err)
	}
}

// renderTable answers a table request: the whole page, or for htmx just the
// results (a new search or sort) or just more rows (scrolling).
func (s *Server) renderTable(
	resp http.ResponseWriter,
	req *http.Request,
	page string,
	state tableState,
	body any,
) {
	if req.Header.Get("Hx-Request") != "true" {
		s.render(resp, page, view{Title: page, Nav: page, Body: body})

		return
	}

	fragment := "results"
	if state.Offset > 0 {
		fragment = "rows"
	}

	s.renderFragment(resp, page, fragment, body)
}

func (s *Server) renderFragment(resp http.ResponseWriter, page, fragment string, data any) {
	resp.Header().Set("Content-Type", "text/html; charset=utf-8")

	if err := s.pages[page].ExecuteTemplate(resp, fragment, data); err != nil {
		s.log.Error("render fragment", "page", page, "fragment", fragment, "err", err)
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

func (s *Server) decisions(resp http.ResponseWriter, req *http.Request) {
	query := req.URL.Query()
	state := parseTableState("/", query, decisionSorts())

	verdict := query.Get("verdict")
	if !slices.Contains(verdicts(), verdict) {
		verdict = ""
	}

	state.Filters["verdict"] = verdict

	onlyID, err := strconv.ParseInt(query.Get("id"), 10, 64)
	if err != nil || onlyID < 1 {
		onlyID = 0
	} else {
		state.Filters["id"] = strconv.FormatInt(onlyID, 10)
	}

	found, total, err := s.db.QueryDecisions(req.Context(), store.DecisionQuery{
		ID: onlyID, Verdict: verdict, Search: state.Search, Sort: state.Sort, Asc: state.Asc,
		Offset: state.Offset, Limit: pageSize,
	})
	if err != nil {
		s.fail(resp, "list decisions", err)

		return
	}

	rows := make([]decisionRow, 0, len(found))
	for _, decision := range found {
		rows = append(rows, decisionRowFor(decision))
	}

	s.renderTable(resp, req, pageDecisions, state, decisionsBody{
		tableState: state,
		Rows:       rows,
		Count: countLabel(
			total,
			"decision",
			"decisions",
			verdict != "" || state.Search != "" || onlyID > 0,
		),
		Verdict:  verdict,
		Verdicts: verdictChoices(verdict),
		Columns:  state.columns(decisionSorts()),
		MoreHref: state.moreHref(len(rows), total),
		OnlyID:   onlyID,
	})
}

func verdictChoices(selected string) []verdictChoice {
	choices := make([]verdictChoice, 0, 1+len(verdicts()))
	choices = append(choices, verdictChoice{Value: "", Label: "All", Checked: selected == ""})

	for _, verdict := range verdicts() {
		choices = append(
			choices,
			verdictChoice{Value: verdict, Label: verdict, Checked: verdict == selected},
		)
	}

	return choices
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

		Undone:      decision.Undone,
		CorrectedBy: decision.CorrectedBy,
	}
}

// actionLabel says what was done; what live mode would do is prefixed. The
// page adds a later undo or correction after it.
func actionLabel(decision store.Decision) string {
	if decision.Action != "none" && !decision.Applied {
		return "would " + decision.Action
	}

	return decision.Action
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

func whitelistSorts() []sortChoice {
	return []sortChoice{
		{store.SortAddress, "Address", true},
		{store.SortSource, "Source", true},
		{store.SortAdded, "Added", false},
	}
}

type whitelistBody struct {
	tableState

	Rows      []addressRow
	Count     string
	Columns   []column
	MoreHref  string
	RemoveAll string
	// Error is why an edit failed, and Note what it did.
	Error string
	Note  string
	// ResetForm asks for the add form to be emptied, which is right after a
	// successful add; ClearConfirm for the removal dialog to be closed.
	ResetForm    bool
	ClearConfirm bool
}

// edit is the outcome of a change to the whitelist, for the page to show.
type edit struct {
	err          error
	note         string
	resetForm    bool
	clearConfirm bool
}

func (s *Server) whitelist(resp http.ResponseWriter, req *http.Request) {
	state := parseTableState("/whitelist", req.URL.Query(), whitelistSorts())

	body, err := s.whitelistBody(req, state, edit{})
	if err != nil {
		s.fail(resp, "list whitelist", err)

		return
	}

	s.renderTable(resp, req, pageWhitelist, state, body)
}

// renderAddresses answers an edit with the refreshed list for the search and
// sort the page had.
func (s *Server) renderAddresses(resp http.ResponseWriter, req *http.Request, change edit) {
	state := parseTableState("/whitelist", req.Form, whitelistSorts())
	state.Offset = 0

	body, err := s.whitelistBody(req, state, change)
	if err != nil {
		s.fail(resp, "list whitelist", err)

		return
	}

	s.renderFragment(resp, pageWhitelist, "results", body)
}

func (s *Server) whitelistBody(
	req *http.Request,
	state tableState,
	change edit,
) (whitelistBody, error) {
	found, total, err := s.db.QueryAddresses(req.Context(), store.AddressQuery{
		Search: state.Search,
		Sort:   state.Sort,
		Desc:   !state.Asc,
		Offset: state.Offset,
		Limit:  pageSize,
	})
	if err != nil {
		return whitelistBody{}, err //nolint:wrapcheck // store errors carry their context
	}

	body := whitelistBody{
		tableState:   state,
		Rows:         make([]addressRow, 0, len(found)),
		Count:        countLabel(total, "address", "addresses", state.Search != ""),
		Columns:      state.columns(whitelistSorts()),
		MoreHref:     state.moreHref(len(found), total),
		RemoveAll:    removeAllLabel(total, state.Search),
		Note:         change.note,
		ResetForm:    change.resetForm,
		ClearConfirm: change.clearConfirm,
	}

	if change.err != nil {
		body.Error = change.err.Error()
	}

	for _, address := range found {
		body.Rows = append(body.Rows, addressRow{
			Address: address.Address, Source: address.Source, Added: localTime(address.AddedAt),
		})
	}

	return body, nil
}

// removeAllLabel names the button that removes every address the table lists,
// or nothing when it lists none.
func removeAllLabel(total int, search string) string {
	switch {
	case total == 0:
		return ""
	case search == "":
		return "Remove all " + strconv.Itoa(total) + "…"
	default:
		return "Remove " + strconv.Itoa(total) + " matching…"
	}
}

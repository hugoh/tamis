package store

import (
	"context"
	"fmt"
	"strings"
)

// Columns a decision query can sort by.
const (
	SortTime    = "time"
	SortID      = "id"
	SortVerdict = "verdict"
	SortReason  = "reason"
	SortScore   = "score"
	SortAction  = "action"
	SortFrom    = "from"
	SortSubject = "subject"
)

const defaultQueryLimit = 100

// DecisionQuery picks and orders journal entries. Its zero value is the
// newest decisions first.
type DecisionQuery struct {
	// Verdict keeps one verdict; empty keeps all.
	Verdict string
	// Search keeps decisions whose sender, subject, reason or action contain
	// it, ignoring ASCII case.
	Search string
	// Sort is one of the Sort constants; anything else sorts by time.
	Sort string
	// Asc sorts upwards; the default is downwards, newest or highest first.
	Asc    bool
	Offset int
	// Limit is the page size; it defaults to 100.
	Limit int
}

// QueryDecisions returns one page of the journal and how many decisions match
// in all.
//
// ponytail: pages are cut by offset, so a decision recorded while someone is
// paging can repeat or skip a row; switch to keyset paging if that matters.
func (s *Store) QueryDecisions(ctx context.Context, query DecisionQuery) ([]Decision, int, error) {
	where, args := query.where()

	var total int

	err := s.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM decisions"+where, args...).Scan(&total)
	if err != nil {
		return nil, 0, fmt.Errorf("count decisions: %w", err)
	}

	limit := query.Limit
	if limit <= 0 {
		limit = defaultQueryLimit
	}

	decisions, err := s.queryDecisions(ctx,
		decisionColumns+" FROM decisions"+where+query.orderBy()+" LIMIT ? OFFSET ?",
		append(args, limit, max(query.Offset, 0))...)

	return decisions, total, err
}

func (q DecisionQuery) where() (string, []any) {
	var (
		conditions []string
		args       []any
	)

	if q.Verdict != "" {
		conditions = append(conditions, "verdict = ?")
		args = append(args, q.Verdict)
	}

	if q.Search != "" {
		pattern := "%" + escapeLike(strings.ToLower(q.Search)) + "%"

		conditions = append(
			conditions,
			`(LOWER(sender) LIKE ? ESCAPE '\' OR LOWER(subject) LIKE ? ESCAPE '\'
			OR LOWER(reason) LIKE ? ESCAPE '\' OR LOWER(action) LIKE ? ESCAPE '\')`,
		)
		args = append(args, pattern, pattern, pattern, pattern)
	}

	if len(conditions) == 0 {
		return "", nil
	}

	return " WHERE " + strings.Join(conditions, " AND "), args
}

func (q DecisionQuery) orderBy() string {
	direction := " DESC"
	if q.Asc {
		direction = " ASC"
	}

	column := sortColumn(q.Sort)

	// Entries that tie on time are in the order they were recorded; on
	// anything else, the newest comes first.
	tiebreak := ", id DESC"
	if column == "at" {
		tiebreak = ", id" + direction
	}

	return " ORDER BY " + column + direction + tiebreak
}

// Columns an address query can sort by.
const (
	SortAddress = "address"
	SortSource  = "source"
	SortAdded   = "added"
)

// AddressQuery picks and orders whitelist entries. Its zero value is every
// address, alphabetically.
type AddressQuery struct {
	// Search keeps addresses or sources that contain it, ignoring ASCII case.
	Search string
	// Sort is one of the address Sort constants; anything else sorts by address.
	Sort string
	// Desc sorts downwards.
	Desc   bool
	Offset int
	// Limit is the page size; it defaults to 100.
	Limit int
}

// QueryAddresses returns one page of the whitelist and how many entries match
// in all.
func (s *Store) QueryAddresses(ctx context.Context, query AddressQuery) ([]Address, int, error) {
	var (
		where string
		args  []any
	)

	if query.Search != "" {
		pattern := "%" + escapeLike(strings.ToLower(query.Search)) + "%"
		where = ` WHERE (LOWER(address) LIKE ? ESCAPE '\' OR LOWER(source) LIKE ? ESCAPE '\')`
		args = []any{pattern, pattern}
	}

	var total int

	if err := s.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM addresses"+where, args...).
		Scan(&total); err != nil {
		return nil, 0, fmt.Errorf("count whitelist: %w", err)
	}

	limit := query.Limit
	if limit <= 0 {
		limit = defaultQueryLimit
	}

	direction := " ASC"
	if query.Desc {
		direction = " DESC"
	}

	entries, err := s.readAddresses(ctx,
		"SELECT address, source, added_at FROM addresses"+where+
			" ORDER BY "+addressSortColumn(query.Sort)+direction+", address ASC LIMIT ? OFFSET ?",
		append(args, limit, max(query.Offset, 0))...)

	return entries, total, err
}

func addressSortColumn(name string) string {
	switch name {
	case SortSource:
		return "source COLLATE NOCASE"
	case SortAdded:
		return "added_at"
	default:
		return "address"
	}
}

// sortColumn maps a sort name to SQL. Only these strings reach the query.
func sortColumn(name string) string {
	switch name {
	case SortID:
		return "id"
	case SortVerdict:
		return "verdict COLLATE NOCASE"
	case SortReason:
		return "reason COLLATE NOCASE"
	case SortScore:
		return "score"
	case SortAction:
		return "action COLLATE NOCASE"
	case SortFrom:
		return "sender COLLATE NOCASE"
	case SortSubject:
		return "subject COLLATE NOCASE"
	default:
		return "at"
	}
}

// escapeLike makes text match literally in a LIKE pattern that uses `\` as its
// escape character.
func escapeLike(text string) string {
	return strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`).Replace(text)
}

// queryDecisions reads the decisions a SELECT built from decisionColumns
// returns.
func (s *Store) queryDecisions(ctx context.Context, query string, args ...any) ([]Decision, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("list decisions: %w", err)
	}

	defer func() { _ = rows.Close() }()

	var decisions []Decision

	for rows.Next() {
		decision, err := scanDecision(rows)
		if err != nil {
			return nil, err
		}

		decisions = append(decisions, decision)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list decisions: %w", err)
	}

	return decisions, nil
}

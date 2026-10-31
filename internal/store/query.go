package store

import (
	"context"
	"encoding/json"
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
	// ID keeps one decision; zero keeps all.
	ID int64
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

	if q.ID > 0 {
		conditions = append(conditions, "id = ?")
		args = append(args, q.ID)
	}

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
	where, args := addressWhere(query.Search)

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

// addressWhere is the condition that keeps whitelist entries whose address or
// source contain search; with an empty search it keeps them all.
func addressWhere(search string) (string, []any) {
	if search == "" {
		return "", nil
	}

	pattern := "%" + escapeLike(strings.ToLower(search)) + "%"

	return ` WHERE (LOWER(address) LIKE ? ESCAPE '\' OR LOWER(source) LIKE ? ESCAPE '\')`,
		[]any{pattern, pattern}
}

const (
	deleteListed   = `DELETE FROM addresses WHERE address IN (SELECT value FROM json_each(?))`
	deleteAll      = `DELETE FROM addresses`
	deleteMatching = `DELETE FROM addresses
		WHERE (LOWER(address) LIKE ? ESCAPE '\' OR LOWER(source) LIKE ? ESCAPE '\')`
)

// RemoveAddresses takes the given addresses off the whitelist, in one
// statement, and returns how many were on it. Case does not matter.
func (s *Store) RemoveAddresses(ctx context.Context, addresses []string) (int, error) {
	lowered := make([]string, len(addresses))
	for i, address := range addresses {
		lowered[i] = strings.ToLower(address)
	}

	list, err := json.Marshal(lowered)
	if err != nil {
		return 0, fmt.Errorf("remove addresses: %w", err)
	}

	result, err := s.db.ExecContext(ctx, deleteListed, string(list))
	if err != nil {
		return 0, fmt.Errorf("remove addresses: %w", err)
	}

	count, _ := result.RowsAffected()

	return int(count), nil
}

// RemoveAddressesMatching removes every entry a QueryAddresses with the same
// search would list, and returns how many. An empty search matches them all.
func (s *Store) RemoveAddressesMatching(ctx context.Context, search string) (int, error) {
	statement, args := deleteAll, []any(nil)

	if search != "" {
		pattern := "%" + escapeLike(strings.ToLower(search)) + "%"
		statement, args = deleteMatching, []any{pattern, pattern}
	}

	result, err := s.db.ExecContext(ctx, statement, args...)
	if err != nil {
		return 0, fmt.Errorf("remove addresses: %w", err)
	}

	count, _ := result.RowsAffected()

	return int(count), nil
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

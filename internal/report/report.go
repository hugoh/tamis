// Package report holds what the health and probe reports share.
package report

import (
	"encoding/json"
	"fmt"
	"io"
	"text/tabwriter"
)

// WriteJSON renders v as indented JSON.
func WriteJSON(w io.Writer, v any) error {
	enc := json.NewEncoder(w)
	enc.SetIndent("", "  ")

	if err := enc.Encode(v); err != nil {
		return fmt.Errorf("write report: %w", err)
	}

	return nil
}

const columnPadding = 2

// WriteTable lays out the rows fill writes, tab-separated, in aligned columns.
func WriteTable(w io.Writer, fill func(table io.Writer)) error {
	table := tabwriter.NewWriter(w, 0, 0, columnPadding, ' ', 0)

	fill(table)

	if err := table.Flush(); err != nil {
		return fmt.Errorf("write report: %w", err)
	}

	return nil
}

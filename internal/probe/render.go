package probe

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"text/tabwriter"
)

const columnPadding = 2

// label renders a status so problems stand out in a terminal.
func label(status Status) string {
	if status == StatusWarn || status == StatusFail {
		return strings.ToUpper(string(status))
	}

	return string(status)
}

// WriteText renders the report for a terminal.
func (r *Report) WriteText(w io.Writer) error {
	table := tabwriter.NewWriter(w, 0, 0, columnPadding, ' ', 0)

	_, _ = fmt.Fprintf(table, "Server: %s\n", r.Server)
	_, _ = fmt.Fprintf(table, "Capabilities: %s\n\n", strings.Join(r.Capabilities, " "))

	for _, c := range r.Checks {
		_, _ = fmt.Fprintf(table, "%s\t%s\t%s\n", label(c.Status), c.Name, c.Detail)
	}

	if len(r.Mailboxes) > 0 {
		_, _ = fmt.Fprintln(table, "\nFolders:")

		for _, m := range r.Mailboxes {
			_, _ = fmt.Fprintf(table, "  %s\t%s\n", m.Name, strings.Join(m.Attrs, " "))
		}
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

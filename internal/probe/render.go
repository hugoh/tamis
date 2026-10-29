package probe

import (
	"fmt"
	"io"
	"strings"

	"github.com/hugoh/tamis/internal/report"
)

// label renders a status so problems stand out in a terminal.
func label(status Status) string {
	if status == StatusWarn || status == StatusFail {
		return strings.ToUpper(string(status))
	}

	return string(status)
}

// WriteText renders the report for a terminal.
func (r *Report) WriteText(w io.Writer) error {
	return report.WriteTable( //nolint:wrapcheck // the error already says "write report"
		w, func(table io.Writer) {
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
		},
	)
}

// WriteJSON renders the report as indented JSON.
func (r *Report) WriteJSON(w io.Writer) error {
	return report.WriteJSON(w, r) //nolint:wrapcheck // the error already says "write report"
}

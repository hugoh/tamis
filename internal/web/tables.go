package web

import (
	"net/url"
	"strconv"
	"strings"
)

const (
	// pageSize is how many rows a table loads at a time.
	pageSize = 100
	// maxOffset bounds a table's offset, so a URL can't ask for the moon.
	maxOffset = 10_000_000

	ascending  = "asc"
	descending = "desc"
)

// sortChoice is a column a table can be sorted by.
type sortChoice struct {
	Key, Label string
	// AscFirst is the direction a first click sorts in.
	AscFirst bool
}

// column is a table header.
type column struct {
	Label, Href, AriaSort, Arrow string
}

// tableState is what a table's URL carries: its search, its sort and where
// the page starts, plus any filters the page adds.
type tableState struct {
	Path    string
	Filters map[string]string
	Search  string
	Sort    string
	Asc     bool
	Offset  int
}

// parseTableState reads a table's state from a query string or a form,
// ignoring anything unknown.
func parseTableState(path string, values url.Values, choices []sortChoice) tableState {
	state := tableState{
		Path:    path,
		Filters: map[string]string{},
		Search:  strings.TrimSpace(values.Get("q")),
		Sort:    choices[0].Key,
		Asc:     choices[0].AscFirst,
	}

	for _, choice := range choices {
		if choice.Key == values.Get("sort") {
			state.Sort, state.Asc = choice.Key, choice.AscFirst
		}
	}

	switch values.Get("dir") {
	case ascending:
		state.Asc = true
	case descending:
		state.Asc = false
	}

	if offset, err := strconv.Atoi(values.Get("offset")); err == nil && offset > 0 {
		state.Offset = min(offset, maxOffset)
	}

	return state
}

func (t tableState) Dir() string {
	if t.Asc {
		return ascending
	}

	return descending
}

func (t tableState) href(sort string, asc bool, offset int) string {
	values := url.Values{}

	for name, value := range t.Filters {
		if value != "" {
			values.Set(name, value)
		}
	}

	if t.Search != "" {
		values.Set("q", t.Search)
	}

	dir := descending
	if asc {
		dir = ascending
	}

	values.Set("sort", sort)
	values.Set("dir", dir)

	if offset > 0 {
		values.Set("offset", strconv.Itoa(offset))
	}

	return t.Path + "?" + values.Encode()
}

// columns builds the headers: the sorted one shows its direction and flips it
// when clicked; the others sort the way their choice says.
func (t tableState) columns(choices []sortChoice) []column {
	headers := make([]column, 0, len(choices))

	for _, choice := range choices {
		header := column{Label: choice.Label, AriaSort: "none"}

		if choice.Key == t.Sort {
			header.Href = t.href(choice.Key, !t.Asc, 0)
			header.AriaSort, header.Arrow = "descending", "▼"

			if t.Asc {
				header.AriaSort, header.Arrow = "ascending", "▲"
			}
		} else {
			header.Href = t.href(choice.Key, choice.AscFirst, 0)
		}

		headers = append(headers, header)
	}

	return headers
}

// moreHref is where the next page of rows comes from, or empty when this was
// the last.
func (t tableState) moreHref(shown, total int) string {
	next := t.Offset + shown
	if next >= total {
		return ""
	}

	return t.href(t.Sort, t.Asc, next)
}

// countLabel says how many rows a table has, for example "2 decisions match".
func countLabel(total int, one, many string, filtered bool) string {
	noun := many
	if total == 1 {
		noun = one
	}

	label := strconv.Itoa(total) + " " + noun
	if !filtered {
		return label
	}

	if total == 1 {
		return label + " matches"
	}

	return label + " match"
}

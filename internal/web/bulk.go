package web

import (
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strconv"
	"strings"

	"github.com/hugoh/tamis/internal/store"
)

// confirmShown is how many addresses the removal dialog lists; the count above
// them says how many there are in all.
const confirmShown = 200

var (
	errNothingSelected = errors.New("select at least one address")
	errNothingMatches  = errors.New("no addresses match")
	errListChanged     = errors.New("the list changed since you reviewed it; nothing was removed")
)

// removal is what a request asks to take off the whitelist: the addresses it
// names, or everything a search matches.
type removal struct {
	all    bool
	search string
	// addresses are the ones named, normalised, sorted and without repeats.
	addresses []string
	// count is how many addresses would go.
	count int
}

func (s *Server) parseRemoval(req *http.Request) (removal, error) {
	if err := req.ParseForm(); err != nil {
		return removal{}, errNothingSelected
	}

	if req.FormValue("scope") == "all" {
		search := strings.TrimSpace(req.FormValue("q"))

		_, total, err := s.db.QueryAddresses(
			req.Context(),
			store.AddressQuery{Search: search, Limit: 1},
		)
		if err != nil {
			return removal{}, err //nolint:wrapcheck // store errors carry their context
		}

		if total == 0 {
			return removal{}, errNothingMatches
		}

		return removal{all: true, search: search, count: total}, nil
	}

	var addresses []string

	for _, given := range req.Form["address"] {
		address, err := store.ParseAddress(given)
		if err != nil {
			return removal{}, err //nolint:wrapcheck // the error names the address
		}

		addresses = append(addresses, address)
	}

	slices.Sort(addresses)
	addresses = slices.Compact(addresses)

	if len(addresses) == 0 {
		return removal{}, errNothingSelected
	}

	return removal{addresses: addresses, count: len(addresses)}, nil
}

// confirmBody is the removal dialog.
type confirmBody struct {
	Title string
	Shown []string
	More  int
	// Selected are the addresses to send back when confirmed, or All and Search
	// say to remove whatever matches.
	Selected []string
	All      bool
	Search   string
	Count    int
	// Error is why there is nothing to confirm.
	Error string
}

// whitelistConfirm answers "remove these?" with a dialog listing what would go.
// Nothing is removed until the dialog is confirmed.
func (s *Server) whitelistConfirm(resp http.ResponseWriter, req *http.Request) {
	pending, err := s.parseRemoval(req)
	if err != nil {
		if isInternal(err) {
			s.fail(resp, "review removal", err)

			return
		}

		s.renderFragment(resp, pageWhitelist, "confirm", confirmBody{Error: err.Error()})

		return
	}

	body := confirmBody{All: pending.all, Search: pending.search, Count: pending.count}

	if pending.all {
		found, _, err := s.db.QueryAddresses(req.Context(), store.AddressQuery{
			Search: pending.search, Limit: confirmShown,
		})
		if err != nil {
			s.fail(resp, "review removal", err)

			return
		}

		for _, entry := range found {
			body.Shown = append(body.Shown, entry.Address)
		}
	} else {
		body.Shown = pending.addresses[:min(len(pending.addresses), confirmShown)]
		body.Selected = pending.addresses
	}

	body.More = pending.count - len(body.Shown)
	body.Title = removalTitle(pending)

	s.renderFragment(resp, pageWhitelist, "confirm", body)
}

func removalTitle(pending removal) string {
	count := strconv.Itoa(pending.count)

	switch {
	case !pending.all && pending.count == 1:
		return "Remove 1 address?"
	case !pending.all:
		return "Remove " + count + " addresses?"
	case pending.search == "":
		return "Remove all " + count + " addresses?"
	case pending.count == 1:
		return "Remove the 1 address matching “" + pending.search + "”?"
	default:
		return "Remove the " + count + " addresses matching “" + pending.search + "”?"
	}
}

func (s *Server) whitelistCancel(resp http.ResponseWriter, _ *http.Request) {
	s.renderFragment(resp, pageWhitelist, "confirm", confirmBody{})
}

// whitelistRemove removes what a confirmed dialog (or a plain request) names.
// The whole request is checked before anything is removed.
func (s *Server) whitelistRemove(resp http.ResponseWriter, req *http.Request) {
	pending, err := s.parseRemoval(req)
	if err != nil && isInternal(err) {
		s.fail(resp, "remove addresses", err)

		return
	}

	removed := 0

	if err == nil {
		removed, err = s.remove(req, pending)
	}

	change := edit{err: err, clearConfirm: true}
	if err == nil {
		change.note = "Removed " + strconv.Itoa(removed) + " address"
		if removed != 1 {
			change.note += "es"
		}
	}

	s.renderAddresses(resp, req, change)
}

func (s *Server) remove(req *http.Request, pending removal) (int, error) {
	if !pending.all {
		removed, err := s.db.RemoveAddresses(req.Context(), pending.addresses)
		if err != nil {
			return 0, fmt.Errorf("remove addresses: %w", err)
		}

		return removed, nil
	}

	if expected, err := strconv.Atoi(
		req.FormValue("count"),
	); err != nil ||
		expected != pending.count {
		return 0, errListChanged
	}

	removed, err := s.db.RemoveAddressesMatching(req.Context(), pending.search)
	if err != nil {
		return 0, fmt.Errorf("remove matching addresses: %w", err)
	}

	return removed, nil
}

// isInternal tells a failure of the server from a request that was wrong.
func isInternal(err error) bool {
	return !errors.Is(err, errNothingSelected) && !errors.Is(err, errNothingMatches) &&
		!errors.Is(err, store.ErrInvalidAddress)
}

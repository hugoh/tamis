package web

import (
	"context"
	"errors"
	"net/http"
	"strconv"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/store"
)

const manualSource = "manual"

// Actions is what the web interface asks of the engine, which needs a live
// mailbox connection.
type Actions interface {
	Undo(ctx context.Context, decisionID int64) (string, error)
	Learn(ctx context.Context, decisionID int64, label *classify.Label) (string, error)
}

// RunActions connects, calls body with the connection's Actions and
// disconnects.
type RunActions func(ctx context.Context, body func(context.Context, Actions) error) error

// decisionAction runs act on the decision the path names and answers with its
// refreshed table row.
func (s *Server) decisionAction(
	act func(context.Context, Actions, int64) (string, error),
) http.HandlerFunc {
	return func(resp http.ResponseWriter, req *http.Request) {
		decisionID, err := strconv.ParseInt(req.PathValue("id"), 10, 64)
		if err != nil {
			http.Error(resp, "bad decision id", http.StatusBadRequest)

			return
		}

		if _, err := s.db.Decision(req.Context(), decisionID); errors.Is(err, store.ErrNotFound) {
			http.Error(resp, "no such decision", http.StatusNotFound)

			return
		} else if err != nil {
			s.fail(resp, "find decision", err)

			return
		}

		s.actionMu.Lock()
		// A closed tab must not stop an undo between moving the mail and
		// recording that it moved.
		summary, err := s.runAction(context.WithoutCancel(req.Context()), decisionID, act)
		s.actionMu.Unlock()

		note := summary

		if err != nil {
			s.log.Warn("decision action failed", "decision", decisionID, "err", err)

			note = err.Error()
		}

		decision, lookupErr := s.db.Decision(req.Context(), decisionID)
		if lookupErr != nil {
			s.fail(resp, "reload decision", lookupErr)

			return
		}

		row := decisionRowFor(decision)
		row.Note, row.Failed = note, err != nil

		resp.Header().Set("Content-Type", "text/html; charset=utf-8")

		if err := s.pages[pageDecisions].ExecuteTemplate(resp, "row", row); err != nil {
			s.log.Error("render row", "err", err)
		}
	}
}

func (s *Server) runAction(
	ctx context.Context,
	decisionID int64,
	act func(context.Context, Actions, int64) (string, error),
) (string, error) {
	var summary string

	err := s.run(ctx, func(ctx context.Context, actions Actions) error {
		var err error

		summary, err = act(ctx, actions, decisionID)

		return err
	})

	return summary, err
}

func (s *Server) undo(resp http.ResponseWriter, req *http.Request) {
	s.decisionAction(func(ctx context.Context, actions Actions, decisionID int64) (string, error) {
		return actions.Undo(ctx, decisionID)
	})(resp, req)
}

func (s *Server) learn(resp http.ResponseWriter, req *http.Request) {
	var label *classify.Label

	switch req.URL.Query().Get("label") {
	case "":
	case "good":
		label = new(classify.Good)
	case "spam":
		label = new(classify.Spam)
	default:
		http.Error(resp, "label must be good or spam", http.StatusBadRequest)

		return
	}

	s.decisionAction(func(ctx context.Context, actions Actions, decisionID int64) (string, error) {
		return actions.Learn(ctx, decisionID, label)
	})(resp, req)
}

func (s *Server) whitelistAdd(resp http.ResponseWriter, req *http.Request) {
	address, err := store.ParseAddress(req.FormValue("address"))
	if err == nil {
		err = s.db.AddAddress(req.Context(), address, manualSource)
	}

	s.renderAddresses(resp, req, edit{err: err, resetForm: err == nil})
}

// learnable and undoable mirror the engine's own rules, so the page offers
// only what will work.
func learnable(decision store.Decision) bool {
	return !decision.Applied && decision.CorrectedBy == 0 && ownJudgement(decision)
}

func undoable(decision store.Decision) bool {
	return decision.Applied && !decision.Undone && ownJudgement(decision)
}

func ownJudgement(decision store.Decision) bool {
	return decision.Reason == engine.ReasonBayes || decision.Reason == engine.ReasonWhitelisted
}

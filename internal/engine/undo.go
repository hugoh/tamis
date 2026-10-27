package engine

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/store"
)

// ReasonUndo marks journal entries written by Undo.
const ReasonUndo = "undo"

// Undo failures, besides store.ErrNotFound and store.ErrAlreadyUndone.
var (
	// ErrNothingToUndo means the decision was only recorded (shadow mode, or
	// a verdict with no action).
	ErrNothingToUndo = errors.New("nothing to undo: the action was never applied")
	// ErrNotUndoable means the decision isn't one of tamis's own filings;
	// training is changed by dropping the message in the other training
	// folder.
	ErrNotUndoable = errors.New("only tamis's own filing can be undone; " +
		"to retrain, drop the message in the other training folder")
	// ErrMessageGone means the message is no longer where tamis put it.
	ErrMessageGone = errors.New("message not found where tamis left it; was it moved or deleted?")
)

// Undo reverses a decision tamis applied:
//   - mail moved to Junk is learnt as good, its sender whitelisted, and
//     moved back to the folder it came from;
//   - mail flagged uncertain has the flag cleared.
//
// It returns a one-line account of what it did. A failed undo leaves the
// decision as it was, so it can be retried.
func (e *Engine) Undo(ctx context.Context, decisionID int64) (string, error) {
	decision, err := e.store.Decision(ctx, decisionID)
	if err != nil {
		return "", fmt.Errorf("undo: %w", err)
	}

	undo, err := e.undoerFor(decision)
	if err != nil {
		return "", fmt.Errorf("undo decision %d: %w", decisionID, err)
	}

	summary, err := undo(ctx, decision)
	if err != nil {
		return "", fmt.Errorf("undo decision %d: %w", decisionID, err)
	}

	if err := e.store.MarkUndone(ctx, decisionID); err != nil {
		return "", fmt.Errorf("undo decision %d: %w", decisionID, err)
	}

	return summary, nil
}

type undoer func(context.Context, store.Decision) (string, error)

// undoerFor says how to reverse a decision, or why it can't be.
func (e *Engine) undoerFor(decision store.Decision) (undoer, error) {
	switch {
	case decision.Undone:
		return nil, store.ErrAlreadyUndone
	case !decision.Applied:
		return nil, ErrNothingToUndo
	case decision.Reason != ReasonBayes && decision.Reason != ReasonWhitelisted:
		return nil, ErrNotUndoable
	case strings.HasPrefix(decision.Action, "move:"):
		return e.undoMove, nil
	case decision.Action == "keyword:"+UncertainKeyword:
		return e.undoUncertain, nil
	default:
		return nil, ErrNotUndoable
	}
}

// undoMove brings a message back to where it was before tamis moved it,
// teaching tamis it was wrong: filed spam is learnt as good and its sender
// whitelisted; a rescue is learnt as spam and its sender dropped from the
// whitelist.
func (e *Engine) undoMove(ctx context.Context, decision store.Decision) (string, error) {
	movedTo := strings.TrimPrefix(decision.Action, "move:")

	reversal := moveReversal{keyword: FiledKeyword, label: classify.Good, verdict: VerdictGood}
	if decision.Verdict == VerdictGood {
		reversal = moveReversal{keyword: RescuedKeyword, label: classify.Spam, verdict: VerdictSpam}
	}

	messages, err := e.locate(movedTo, decision.MessageID)
	if err != nil {
		return "", err
	}

	uids := make([]uint32, len(messages))

	for i, message := range messages {
		uids[i] = message.UID

		if err := e.relearn(ctx, message, reversal.label); err != nil {
			return "", err
		}
	}

	if err := e.mail.RemoveKeyword(movedTo, uids, reversal.keyword); err != nil {
		return "", fmt.Errorf("clear %s: %w", reversal.keyword, err)
	}

	if err := e.mail.Move(movedTo, uids, decision.Mailbox); err != nil {
		return "", fmt.Errorf("move back: %w", err)
	}

	err = e.record(ctx, decision.Mailbox, messages[0],
		judgement{verdict: reversal.verdict, reason: ReasonUndo}, "move:"+decision.Mailbox, true)
	if err != nil {
		return "", err
	}

	if reversal.label == classify.Spam {
		return fmt.Sprintf("moved back to %s, learnt as spam, sender removed from whitelist",
			decision.Mailbox), nil
	}

	return fmt.Sprintf(
		"moved back to %s, learnt as good, sender whitelisted",
		decision.Mailbox,
	), nil
}

// moveReversal is what undoing a move teaches: the keyword tamis set, and
// the label and verdict the message really deserved.
type moveReversal struct {
	keyword string
	label   classify.Label
	verdict string
}

// relearn trains message as label and updates the whitelist to match.
func (e *Engine) relearn(ctx context.Context, message mail.Message, label classify.Label) error {
	if _, err := e.store.Train(
		ctx,
		trainingKey(message),
		classify.Tokenize(message.Raw),
		label,
	); err != nil {
		return fmt.Errorf("learn %s: %w", label, err)
	}

	if message.From == "" {
		return nil
	}

	var err error
	if label == classify.Good {
		err = e.store.AddAddress(ctx, message.From, ReasonUndo)
	} else {
		err = e.store.RemoveAddress(ctx, message.From)
	}

	if err != nil {
		return fmt.Errorf("update whitelist: %w", err)
	}

	return nil
}

func (e *Engine) undoUncertain(ctx context.Context, decision store.Decision) (string, error) {
	messages, err := e.locate(decision.Mailbox, decision.MessageID)
	if err != nil {
		return "", err
	}

	uids := make([]uint32, len(messages))
	for i, message := range messages {
		uids[i] = message.UID
	}

	if err := e.mail.RemoveKeyword(decision.Mailbox, uids, UncertainKeyword); err != nil {
		return "", fmt.Errorf("clear %s: %w", UncertainKeyword, err)
	}

	err = e.record(ctx, decision.Mailbox, messages[0],
		judgement{verdict: VerdictUncertain, reason: ReasonUndo}, "unflag:"+UncertainKeyword, true)
	if err != nil {
		return "", err
	}

	return "cleared " + UncertainKeyword, nil
}

// locate fetches the messages in folder carrying messageID.
func (e *Engine) locate(folder, messageID string) ([]mail.Message, error) {
	uids, err := e.mail.FindMessageID(folder, messageID)
	if err != nil {
		return nil, fmt.Errorf("find message in %s: %w", folder, err)
	}

	if len(uids) == 0 {
		return nil, fmt.Errorf("%s in %s: %w", messageID, folder, ErrMessageGone)
	}

	messages, err := e.mail.Fetch(folder, uids)
	if err != nil {
		return nil, fmt.Errorf("fetch from %s: %w", folder, err)
	}

	if len(messages) == 0 {
		return nil, fmt.Errorf("%s in %s: %w", messageID, folder, ErrMessageGone)
	}

	return messages, nil
}

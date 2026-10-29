package engine

import (
	"context"
	"errors"
	"fmt"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/store"
)

// Learn failures, besides store.ErrNotFound and ErrMessageGone.
var (
	// ErrAlreadyApplied means tamis acted on the decision, so undo is the
	// way to change it.
	ErrAlreadyApplied = errors.New("the action was applied; use undo to reverse it")
	// ErrNotLearnable means the decision isn't one of tamis's own judgements.
	ErrNotLearnable = errors.New("only tamis's own judgements can be learnt from; " +
		"to retrain, drop the message in a training folder")
	// ErrAlreadyCorrected means the decision has a later correction; ask for a
	// label to change it again.
	ErrAlreadyCorrected = errors.New("already corrected")
	// ErrNeedsLabel means an uncertain verdict has no opposite.
	ErrNeedsLabel = errors.New("an uncertain verdict needs an explicit label: good or spam")
)

// Learn teaches tamis the right answer for a decision it only recorded, such
// as one made in shadow mode. With no label it learns the opposite of the
// verdict. The message stays where it is. It returns a one-line account of
// what it did.
func (e *Engine) Learn(
	ctx context.Context,
	decisionID int64,
	label *classify.Label,
) (string, error) {
	decision, err := e.store.Decision(ctx, decisionID)
	if err != nil {
		return "", fmt.Errorf("learn: %w", err)
	}

	learnt, err := learntLabel(decision, label)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	messages, err := e.locate(decision.Mailbox, decision.MessageID)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	if err := e.relearn(ctx, messages[0], learnt); err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	verdict := VerdictGood
	if learnt == classify.Spam {
		verdict = VerdictSpam
	}

	err = e.record(ctx, decision.Mailbox, messages[0],
		judgement{verdict: verdict, reason: ReasonCorrection}, "train:"+learnt.String(), true)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	if learnt == classify.Spam {
		return "learnt as spam, sender removed from whitelist", nil
	}

	return "learnt as good, sender whitelisted", nil
}

func learntLabel(decision store.Decision, label *classify.Label) (classify.Label, error) {
	switch {
	case decision.Reason != ReasonBayes && decision.Reason != ReasonWhitelisted:
		return 0, ErrNotLearnable
	case decision.Applied:
		return 0, ErrAlreadyApplied
	case label == nil && decision.CorrectedBy > 0:
		return 0, fmt.Errorf("%w by decision %d; pass --good or --spam to change it",
			ErrAlreadyCorrected, decision.CorrectedBy)
	case label != nil:
		return *label, nil
	case decision.Verdict == VerdictSpam:
		return classify.Good, nil
	case decision.Verdict == VerdictGood:
		return classify.Spam, nil
	default:
		return 0, ErrNeedsLabel
	}
}

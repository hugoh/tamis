package engine

import (
	"context"
	"errors"
	"fmt"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/mail"
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

	folder, messages, err := e.locateForLearn(decision)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	if err := e.relearn(ctx, messages[0], learnt); err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	score, err := e.rescore(ctx, messages[0].Raw)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	verdict := VerdictGood
	if learnt == classify.Spam {
		verdict = VerdictSpam
	}

	err = e.record(
		ctx,
		folder,
		messages[0],
		judgement{
			verdict: verdict,
			reason:  ReasonCorrection,
			score:   score,
		},
		"train:"+learnt.String(),
		true,
	)
	if err != nil {
		return "", fmt.Errorf("learn decision %d: %w", decisionID, err)
	}

	if learnt == classify.Spam {
		return "learnt as spam, sender removed from whitelist", nil
	}

	return "learnt as good, sender whitelisted", nil
}

// locateForLearn finds the message where it was judged or, if the user has
// since sent it to Junk, there.
func (e *Engine) locateForLearn(decision store.Decision) (string, []mail.Message, error) {
	var err error

	for _, folder := range []string{decision.Mailbox, e.cfg.Folders.Junk} {
		var messages []mail.Message

		messages, err = e.locate(folder, decision.MessageID)
		if !errors.Is(err, ErrMessageGone) {
			return folder, messages, err
		}
	}

	return "", nil, err
}

func learntLabel(decision store.Decision, label *classify.Label) (classify.Label, error) {
	if err := refuseLearning(decision, label); err != nil {
		return 0, err
	}

	switch {
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

// refuseLearning says why a decision can't be learnt from. A correction is
// already applied by nature, so it stays learnable to change one's mind.
func refuseLearning(decision store.Decision, label *classify.Label) error {
	correction := decision.Reason == ReasonCorrection

	switch {
	case decision.Reason != ReasonBayes && decision.Reason != ReasonWhitelisted && !correction:
		return ErrNotLearnable
	case decision.Applied && !correction:
		return ErrAlreadyApplied
	case label == nil && decision.CorrectedBy > 0:
		return fmt.Errorf("%w by decision %d; pass --good or --spam to change it",
			ErrAlreadyCorrected, decision.CorrectedBy)
	default:
		return nil
	}
}

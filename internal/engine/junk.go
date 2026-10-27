package engine

import (
	"context"
	"fmt"
	"slices"
	"strings"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/mail"
)

// learnFromJunk treats a message tamis judged good or uncertain in a watched
// folder, now arriving in Junk, as one the user moved there: missed spam.
// Everything else in Junk (filed by the provider, by tamis, or through
// TrainSpam) is left alone.
func (e *Engine) learnFromJunk(ctx context.Context, message mail.Message, summary *Summary) error {
	missed, err := e.isMissedSpam(ctx, message)
	if err != nil || !missed {
		return err
	}

	summary.Corrected++

	return e.learnSpam(
		ctx,
		e.cfg.Folders.Junk,
		message,
		classify.Tokenize(message.Raw),
		ReasonCorrection,
	)
}

// isMissedSpam reports whether the user moved this message to Junk after
// tamis let it through.
func (e *Engine) isMissedSpam(ctx context.Context, message mail.Message) (bool, error) {
	if slices.Contains(message.Flags, strings.ToLower(FiledKeyword)) {
		return false, nil
	}

	label, trained, err := e.store.Trained(ctx, trainingKey(message))
	if err != nil {
		return false, fmt.Errorf("check training: %w", err)
	}

	if trained && label == classify.Spam {
		return false, nil
	}

	decision, found, err := e.store.LatestDecision(ctx, message.MessageID)
	if err != nil {
		return false, fmt.Errorf("look up decision: %w", err)
	}

	return found &&
		(decision.Verdict == VerdictGood || decision.Verdict == VerdictUncertain) &&
		slices.Contains(e.cfg.Folders.Watch, decision.Mailbox), nil
}

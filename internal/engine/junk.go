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
func (e *Engine) learnFromJunk(
	ctx context.Context,
	message mail.Message,
	summary *Summary,
) (bool, error) {
	missed, err := e.isMissedSpam(ctx, message)
	if err != nil || !missed {
		return false, err
	}

	summary.Corrected++

	tokens, err := e.tokens(ctx, message.Raw)
	if err != nil {
		return false, err
	}

	return true, e.learnSpam(ctx, e.cfg.Folders.Junk, message, tokens, ReasonCorrection)
}

// handleJunk learns from what the user moved to Junk, then considers
// rescuing what the provider put there, and learns from what stays.
func (e *Engine) handleJunk(ctx context.Context, message mail.Message, summary *Summary) error {
	if e.cfg.Policy.LearnFromJunk {
		if learnt, err := e.learnFromJunk(ctx, message, summary); err != nil || learnt {
			return err
		}
	}

	if e.cfg.Policy.Rescue {
		if rescued, err := e.rescue(ctx, message, summary); err != nil || rescued {
			return err
		}
	}

	if e.cfg.Policy.LearnProviderJunk {
		return e.learnProviderJunk(ctx, message, summary)
	}

	return nil
}

// learnProviderJunk trains mail the provider filed to Junk as spam when tamis
// isn't already sure it is: only those teach the model something. It trains
// the tokens and leaves the whitelist alone, since a forged message from a
// contact lands here too.
func (e *Engine) learnProviderJunk(
	ctx context.Context,
	message mail.Message,
	summary *Summary,
) error {
	filed, err := e.isProviderFiled(ctx, message)
	if err != nil || !filed {
		return err
	}

	tokens, model, err := e.features(ctx, message.Raw)
	if err != nil {
		return fmt.Errorf("learn provider junk: %w", err)
	}

	score := classify.Score(model, tokens).Score
	if score >= e.cfg.Policy.SpamThreshold {
		return nil
	}

	if _, err := e.store.Train(ctx, trainingKey(message), tokens, classify.Spam); err != nil {
		return fmt.Errorf("learn provider junk: %w", err)
	}

	summary.Trained++

	return e.record(ctx, e.cfg.Folders.Junk, message,
		judgement{verdict: VerdictSpam, reason: ReasonProviderJunk, score: score},
		"train:spam", true)
}

// isMissedSpam reports whether the user moved this message to Junk after
// tamis let it through.
func (e *Engine) isMissedSpam(ctx context.Context, message mail.Message) (bool, error) {
	if slices.Contains(message.Flags, strings.ToLower(FiledKeyword)) {
		return false, nil
	}

	if rescued, err := e.isRescueMovedBack(ctx, message); err != nil || rescued {
		return rescued, err
	}

	label, trained, err := e.store.Trained(ctx, trainingKey(message))
	if err != nil {
		return false, fmt.Errorf("check training: %w", err)
	}

	if trained && label == classify.Spam {
		return false, nil
	}

	return e.wasLetThrough(ctx, message)
}

// wasLetThrough reports whether tamis's latest verdict on the message, in a
// watched folder, left it there.
func (e *Engine) wasLetThrough(ctx context.Context, message mail.Message) (bool, error) {
	decision, found, err := e.store.LatestDecision(ctx, message.MessageID)
	if err != nil {
		return false, fmt.Errorf("look up decision: %w", err)
	}

	return found &&
		(decision.Verdict == VerdictGood || decision.Verdict == VerdictUncertain) &&
		slices.Contains(e.cfg.Folders.Watch, decision.Mailbox), nil
}

// isRescueMovedBack reports whether this is mail tamis rescued from Junk that
// the user put back.
func (e *Engine) isRescueMovedBack(ctx context.Context, message mail.Message) (bool, error) {
	if !slices.Contains(message.Flags, strings.ToLower(RescuedKeyword)) {
		return false, nil
	}

	return e.movedByTamis(ctx, message, VerdictGood)
}

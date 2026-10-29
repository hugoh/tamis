package engine

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/mail"
)

// rescue moves mail the provider filed to Junk back out when its sender is
// whitelisted and authenticated, or (if enabled) its score is very low.
// It reports whether it rescued the message.
func (e *Engine) rescue(ctx context.Context, message mail.Message, summary *Summary) (bool, error) {
	candidate, err := e.isProviderFiled(ctx, message)
	if err != nil || !candidate {
		return false, err
	}

	verdict, rescued, err := e.judgeRescue(ctx, message)
	if err != nil || !rescued {
		return false, err
	}

	summary.Rescued++

	dest := e.cfg.Folders.RescueTo()
	action := "move:" + dest
	applied := false

	if e.cfg.Policy.Mode == config.ModeLive {
		if err := e.moveRescued(message.UID, dest); err != nil {
			return false, fmt.Errorf("rescue: %w", err)
		}

		applied = true
	}

	return true, e.record(ctx, e.cfg.Folders.Junk, message, verdict, action, applied)
}

// isProviderFiled reports whether a message in Junk is mail tamis never saw:
// the provider filed it. Anything tamis judged, trained or filed, and
// anything the user marked Junk, was put there deliberately.
func (e *Engine) isProviderFiled(ctx context.Context, message mail.Message) (bool, error) {
	if slices.Contains(message.Flags, strings.ToLower(FiledKeyword)) {
		return false, nil
	}

	if hasAnyFlag(message.Flags, junkMarks()[classify.Spam]) {
		return false, nil
	}

	if _, trained, err := e.store.Trained(ctx, trainingKey(message)); err != nil || trained {
		return false, err //nolint:wrapcheck // store errors carry their context
	}

	_, judged, err := e.store.LatestDecision(ctx, message.MessageID)
	if err != nil {
		return false, fmt.Errorf("look up decision: %w", err)
	}

	return !judged, nil
}

// judgeRescue requires authentication for whitelisted senders whatever
// whitelist_requires_auth says: impersonated contacts are what Junk catches.
func (e *Engine) judgeRescue(ctx context.Context, message mail.Message) (judgement, bool, error) {
	if message.From != "" && auth.Verified(message.Raw, e.cfg.Account.AuthServerID) {
		whitelisted, err := e.store.HasAddress(ctx, message.From)
		if err != nil {
			return judgement{}, false, fmt.Errorf("rescue: %w", err)
		}

		if whitelisted {
			return judgement{verdict: VerdictGood, reason: ReasonWhitelisted}, true, nil
		}
	}

	if e.cfg.Policy.RescueThreshold <= 0 {
		return judgement{}, false, nil
	}

	tokens := classify.Tokenize(message.Raw)

	model, err := e.store.Model(ctx, tokens)
	if err != nil {
		return judgement{}, false, fmt.Errorf("rescue: %w", err)
	}

	score := classify.Score(model, tokens).Score
	if score > e.cfg.Policy.RescueThreshold {
		return judgement{}, false, nil
	}

	return judgement{verdict: VerdictGood, reason: ReasonBayes, score: score}, true, nil
}

func (e *Engine) moveRescued(uid uint32, dest string) error {
	junk := e.cfg.Folders.Junk
	uids := []uint32{uid}

	if err := e.mail.Ensure(dest); err != nil {
		return fmt.Errorf("create %s: %w", dest, err)
	}

	if err := e.mail.AddKeyword(junk, uids, RescuedKeyword); err != nil {
		return fmt.Errorf("flag rescued: %w", err)
	}

	if err := e.mail.Move(junk, uids, dest); err != nil {
		// Left in Junk with the keyword, the next pass would read it as the
		// user moving a rescue back.
		if undo := e.mail.RemoveKeyword(junk, uids, RescuedKeyword); undo != nil {
			err = errors.Join(err, undo)
		}

		return fmt.Errorf("move to %s: %w", dest, err)
	}

	return nil
}

package engine

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/mail"
)

// keywordWindow bounds how far back keyword marks are looked for, so each
// pass's searches stay small.
const keywordWindow = 7 * 24 * time.Hour

// junkMarks are the keywords mail clients set for Junk and Not Junk: the
// IANA-registered ones and Apple Mail's.
func junkMarks() map[classify.Label][]string {
	return map[classify.Label][]string{
		classify.Spam: {"$Junk", "Junk"},
		classify.Good: {"$NotJunk", "NotJunk"},
	}
}

// learnFromKeywords learns from Junk / Not Junk marks that contradict
// tamis's own verdict on recent mail in the watched folders and Junk. Marks
// that agree, or on mail tamis never judged, may come from a client's own
// filter and are ignored.
func (e *Engine) learnFromKeywords(ctx context.Context, summary *Summary) error {
	since := time.Now().Add(-keywordWindow)

	folders := append(slices.Clone(e.cfg.Folders.Watch), e.cfg.Folders.Junk)

	for _, folder := range folders {
		for label, keywords := range junkMarks() {
			uids, err := e.mail.SearchKeywords(folder, keywords, since)
			if err != nil {
				return fmt.Errorf("search %s for %v: %w", folder, keywords, err)
			}

			if err := e.learnMarked(ctx, folder, uids, label, summary); err != nil {
				return err
			}
		}
	}

	return nil
}

// learnMarked decides from headers alone which marked messages to learn,
// and downloads only those.
func (e *Engine) learnMarked(
	ctx context.Context,
	folder string,
	uids []uint32,
	label classify.Label,
	summary *Summary,
) error {
	envelopes, err := e.mail.FetchEnvelopes(folder, uids)
	if err != nil {
		return fmt.Errorf("fetch marked mail in %s: %w", folder, err)
	}

	var learn []uint32

	for _, envelope := range envelopes {
		contradicts, err := e.markContradicts(ctx, envelope, label)
		if err != nil {
			return err
		}

		if contradicts {
			learn = append(learn, envelope.UID)
		}
	}

	messages, err := e.mail.Fetch(folder, learn)
	if err != nil {
		return fmt.Errorf("fetch marked mail in %s: %w", folder, err)
	}

	for _, message := range messages {
		if err := e.learnMark(ctx, folder, message, label); err != nil {
			return err
		}

		summary.Corrected++
	}

	return nil
}

// markContradicts reports whether a message marked label was last judged
// otherwise by tamis in a watched folder. Messages marked both ways are
// ambiguous and skipped.
func (e *Engine) markContradicts(
	ctx context.Context,
	message mail.Message,
	label classify.Label,
) (bool, error) {
	marks := junkMarks()
	if hasAnyFlag(message.Flags, marks[classify.Spam]) &&
		hasAnyFlag(message.Flags, marks[classify.Good]) {
		return false, nil
	}

	decision, found, err := e.store.LatestDecision(ctx, message.MessageID)
	if err != nil {
		return false, fmt.Errorf("look up decision: %w", err)
	}

	if !found || !slices.Contains(e.cfg.Folders.Watch, decision.Mailbox) {
		return false, nil
	}

	if label == classify.Spam {
		return decision.Verdict == VerdictGood || decision.Verdict == VerdictUncertain, nil
	}

	return decision.Verdict == VerdictSpam || decision.Verdict == VerdictUncertain, nil
}

func (e *Engine) learnMark(
	ctx context.Context,
	folder string,
	message mail.Message,
	label classify.Label,
) error {
	tokens, err := e.tokens(ctx, message.Raw)
	if err != nil {
		return err
	}

	if label == classify.Good {
		return e.learnGood(ctx, folder, message, tokens, ReasonCorrection)
	}

	return e.learnSpam(ctx, folder, message, tokens, ReasonCorrection)
}

// hasAnyFlag reports whether flags (lower-cased, as mail.Message has them)
// include any of wanted, compared case-insensitively.
func hasAnyFlag(flags, wanted []string) bool {
	return slices.ContainsFunc(wanted, func(flag string) bool {
		return slices.Contains(flags, strings.ToLower(flag))
	})
}

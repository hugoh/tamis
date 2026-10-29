// Package engine is tamis's sync loop: it trains from the training folders,
// judges new mail in the watched folders, files it (in live mode) and
// journals every decision.
package engine

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"log/slog"
	"slices"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/store"
)

// Keywords tamis sets on messages it acts on.
const (
	// FiledKeyword marks mail tamis moved to Junk. Seeing it outside Junk
	// later means the user disagreed.
	FiledKeyword = "$TamisFiled"
	// UncertainKeyword marks mail tamis couldn't decide on.
	UncertainKeyword = "$TamisUncertain"
	// RescuedKeyword marks mail tamis moved out of Junk. Seeing it back in
	// Junk means the user disagreed.
	RescuedKeyword = "$TamisRescued"
	// TrainedKeyword marks mail tamis already learnt that it moved on to a
	// folder it scans. Senders can't set keywords, unlike a Message-ID.
	TrainedKeyword = "$TamisTrained"
)

// Verdicts and reasons recorded in the journal.
const (
	VerdictSpam      = "spam"
	VerdictUncertain = "uncertain"
	VerdictGood      = "good"

	ReasonBayes        = "bayes"
	ReasonWhitelisted  = "whitelisted"
	ReasonCorrection   = "correction"
	ReasonTrainFolder  = "train folder"
	ReasonProviderJunk = "provider junk"

	actionNone = "none"
)

const fetchBatch = 50

// Mailbox is what the engine needs from an IMAP connection; *mail.Client
// implements it.
type Mailbox interface {
	MailboxReader
	MailboxWriter
}

// MailboxReader finds and reads messages.
type MailboxReader interface {
	State(mailbox string) (mail.State, error)
	UIDsAfter(mailbox string, after uint32) ([]uint32, error)
	SeenBefore(mailbox string, before time.Time) ([]uint32, error)
	FindMessageID(mailbox, messageID string) ([]uint32, error)
	SearchKeywords(mailbox string, keywords []string, since time.Time) ([]uint32, error)
	Fetch(mailbox string, uids []uint32) ([]mail.Message, error)
	FetchEnvelopes(mailbox string, uids []uint32) ([]mail.Message, error)
	Recipients(mailbox string, uids []uint32) ([]string, error)
}

// MailboxWriter changes where messages are and how they are flagged.
type MailboxWriter interface {
	Move(mailbox string, uids []uint32, dest string) error
	AddKeyword(mailbox string, uids []uint32, keyword string) error
	RemoveKeyword(mailbox string, uids []uint32, keyword string) error
	Ensure(mailbox string) error
}

// Engine runs passes over one account.
type Engine struct {
	mail  Mailbox
	store *store.Store
	cfg   *config.Config
	log   *slog.Logger
}

// New returns an engine for the account behind mailbox.
func New(mailbox Mailbox, db *store.Store, cfg *config.Config, logger *slog.Logger) *Engine {
	return &Engine{mail: mailbox, store: db, cfg: cfg, log: logger}
}

// Summary counts what one pass did.
type Summary struct {
	Scanned   int
	Spam      int
	Uncertain int
	Good      int
	Corrected int
	Trained   int
	Rescued   int
}

// RunOnce trains from the training folders, then judges new mail in each
// watched folder.
func (e *Engine) RunOnce(ctx context.Context) (Summary, error) {
	var summary Summary

	for _, folder := range e.trainFolders() {
		if err := e.train(ctx, folder, &summary); err != nil {
			return summary, err
		}
	}

	judge := func(ctx context.Context, folder string, message mail.Message) error {
		return e.handle(ctx, folder, message, &summary)
	}

	for _, folder := range e.cfg.Folders.Watch {
		if err := e.scan(ctx, folder, judge); err != nil {
			return summary, err
		}
	}

	if err := e.scanJunk(ctx, &summary); err != nil {
		return summary, err
	}

	if e.cfg.Policy.LearnFromKeywords {
		if err := e.learnFromKeywords(ctx, &summary); err != nil {
			return summary, err
		}
	}

	return summary, nil
}

// scanJunk hands new arrivals in Junk to handleJunk when any policy wants them.
func (e *Engine) scanJunk(ctx context.Context, summary *Summary) error {
	policy := e.cfg.Policy
	if !policy.LearnFromJunk && !policy.Rescue && !policy.LearnProviderJunk {
		return nil
	}

	return e.scan(ctx, e.cfg.Folders.Junk,
		func(ctx context.Context, _ string, message mail.Message) error {
			return e.handleJunk(ctx, message, summary)
		})
}

// handler processes one newly arrived message in a scanned folder.
type handler func(ctx context.Context, folder string, message mail.Message) error

// scan hands the folder's messages newer than its cursor to handle. A missing or
// stale cursor (new UIDVALIDITY) restarts the folder from its current mail.
func (e *Engine) scan(ctx context.Context, folder string, handle handler) error {
	state, err := e.mail.State(folder)
	if err != nil {
		return fmt.Errorf("scan %s: %w", folder, err)
	}

	cursor, ok, err := e.store.Cursor(ctx, folder)
	if err != nil {
		return fmt.Errorf("scan %s: %w", folder, err)
	}

	if !ok || cursor.UIDValidity != state.UIDValidity {
		e.log.InfoContext(ctx, "starting folder from current mail", "folder", folder)

		return e.startAtCurrent(ctx, folder, state)
	}

	uids, err := e.mail.UIDsAfter(folder, cursor.LastUID)
	if err != nil {
		return fmt.Errorf("scan %s: %w", folder, err)
	}

	for batch := range slices.Chunk(uids, fetchBatch) {
		if err := e.scanBatch(ctx, folder, batch, &cursor, handle); err != nil {
			return err
		}
	}

	return nil
}

// scanBatch handles one fetch's worth of messages, saving the cursor after
// each so an interrupted pass resumes where it stopped.
func (e *Engine) scanBatch(
	ctx context.Context,
	folder string,
	batch []uint32,
	cursor *store.Cursor,
	handle handler,
) error {
	messages, err := e.mail.Fetch(folder, batch)
	if err != nil {
		return fmt.Errorf("scan %s: %w", folder, err)
	}

	for _, message := range messages {
		if err := handle(ctx, folder, message); err != nil {
			return fmt.Errorf("%s UID %d: %w", folder, message.UID, err)
		}

		cursor.LastUID = max(cursor.LastUID, message.UID)
		if err := e.store.SetCursor(ctx, folder, *cursor); err != nil {
			return fmt.Errorf("scan %s: %w", folder, err)
		}
	}

	return nil
}

func (e *Engine) startAtCurrent(ctx context.Context, folder string, state mail.State) error {
	err := e.store.SetCursor(ctx, folder, store.Cursor{
		UIDValidity: state.UIDValidity,
		LastUID:     max(state.UIDNext, 1) - 1,
	})
	if err != nil {
		return fmt.Errorf("start %s: %w", folder, err)
	}

	return nil
}

func (e *Engine) handle(
	ctx context.Context,
	folder string,
	message mail.Message,
	summary *Summary,
) error {
	key := trainingKey(message)

	_, trained, err := e.store.Trained(ctx, key)
	if err != nil {
		return fmt.Errorf("check training: %w", err)
	}

	if why := skipReason(message, trained); why != "" {
		e.log.DebugContext(ctx, "skipping", "folder", folder, "uid", message.UID, "why", why)

		return nil
	}

	tokens := classify.Tokenize(message.Raw)

	if slices.Contains(message.Flags, strings.ToLower(FiledKeyword)) {
		filed, err := e.movedByTamis(ctx, message, VerdictSpam)
		if err != nil {
			return err
		}

		if filed {
			summary.Scanned++
			summary.Corrected++

			return e.learnGood(ctx, folder, message, tokens, ReasonCorrection)
		}
	}

	verdict, err := e.judge(ctx, message, tokens)
	if err != nil {
		return err
	}

	summary.Scanned++
	summary.count(verdict.verdict)

	action, applied, err := e.act(folder, message.UID, verdict.verdict)
	if err != nil {
		return err
	}

	return e.record(ctx, folder, message, verdict, action, applied)
}

// skipReason says why a message needs no judging, or is empty if it does.
func skipReason(message mail.Message, trained bool) string {
	switch {
	case trained && slices.Contains(message.Flags, strings.ToLower(TrainedKeyword)):
		return "already trained"
	case slices.Contains(message.Flags, strings.ToLower(RescuedKeyword)):
		return "rescued by tamis"
	default:
		return ""
	}
}

func (s *Summary) count(verdict string) {
	switch verdict {
	case VerdictSpam:
		s.Spam++
	case VerdictUncertain:
		s.Uncertain++
	default:
		s.Good++
	}
}

type judgement struct {
	verdict string
	reason  string
	score   float64
}

// judge trusts the whitelist when the sender is (if required) verified, and
// otherwise scores the message with the Bayes model.
func (e *Engine) judge(
	ctx context.Context,
	message mail.Message,
	tokens []string,
) (judgement, error) {
	if message.From != "" {
		whitelisted, err := e.store.HasAddress(ctx, message.From)
		if err != nil {
			return judgement{}, fmt.Errorf("judge: %w", err)
		}

		if whitelisted &&
			(!e.cfg.Policy.WhitelistRequiresAuth || auth.Verified(message.Raw, e.cfg.Account.AuthServerID)) {
			return judgement{verdict: VerdictGood, reason: ReasonWhitelisted}, nil
		}
	}

	model, err := e.store.Model(ctx, tokens)
	if err != nil {
		return judgement{}, fmt.Errorf("judge: %w", err)
	}

	score := classify.Score(model, tokens).Score

	verdict := VerdictUncertain

	switch {
	case score >= e.cfg.Policy.SpamThreshold:
		verdict = VerdictSpam
	case score <= e.cfg.Policy.GoodThreshold:
		verdict = VerdictGood
	}

	return judgement{verdict: verdict, reason: ReasonBayes, score: score}, nil
}

// movedByTamis reports whether the journal shows tamis moved this message
// with the given verdict. A keyword alone doesn't: a move that failed on a
// dropped connection leaves its keyword behind.
func (e *Engine) movedByTamis(
	ctx context.Context,
	message mail.Message,
	verdict string,
) (bool, error) {
	decision, found, err := e.store.LatestDecision(ctx, message.MessageID)
	if err != nil {
		return false, fmt.Errorf("look up decision: %w", err)
	}

	return found && decision.Applied && decision.Verdict == verdict &&
		strings.HasPrefix(decision.Action, "move:"), nil
}

// act files a message according to its verdict; in shadow mode it only
// reports what it would have done.
func (e *Engine) act(folder string, uid uint32, verdict string) (string, bool, error) {
	live := e.cfg.Policy.Mode == config.ModeLive
	uids := []uint32{uid}

	switch verdict {
	case VerdictSpam:
		action := "move:" + e.cfg.Folders.Junk
		if !live {
			return action, false, nil
		}

		if err := e.mail.AddKeyword(folder, uids, FiledKeyword); err != nil {
			return action, false, fmt.Errorf("file spam: %w", err)
		}

		if err := e.mail.Move(folder, uids, e.cfg.Folders.Junk); err != nil {
			// Left in place with the keyword, the next pass would mistake the
			// message for one the user moved back out of Junk.
			if undo := e.mail.RemoveKeyword(folder, uids, FiledKeyword); undo != nil {
				err = errors.Join(err, undo)
			}

			return action, false, fmt.Errorf("file spam: %w", err)
		}

		return action, true, nil
	case VerdictUncertain:
		action := "keyword:" + UncertainKeyword
		if !live {
			return action, false, nil
		}

		if err := e.mail.AddKeyword(folder, uids, UncertainKeyword); err != nil {
			return action, false, fmt.Errorf("flag uncertain: %w", err)
		}

		return action, true, nil
	default:
		return actionNone, false, nil
	}
}

// learnGood trains a message as good and whitelists its sender: the user
// has said it isn't spam.
func (e *Engine) learnGood(
	ctx context.Context,
	folder string,
	message mail.Message,
	tokens []string,
	reason string,
) error {
	if _, err := e.store.Train(ctx, trainingKey(message), tokens, classify.Good); err != nil {
		return fmt.Errorf("learn good: %w", err)
	}

	if message.From != "" {
		if err := e.store.AddAddress(ctx, message.From, reason); err != nil {
			return fmt.Errorf("learn good: %w", err)
		}
	}

	return e.record(ctx, folder, message,
		judgement{verdict: VerdictGood, reason: reason}, "train:good", true)
}

// learnSpam trains a message as spam and takes its sender off the
// whitelist: the user has said it is spam.
func (e *Engine) learnSpam(
	ctx context.Context,
	folder string,
	message mail.Message,
	tokens []string,
	reason string,
) error {
	if _, err := e.store.Train(ctx, trainingKey(message), tokens, classify.Spam); err != nil {
		return fmt.Errorf("learn spam: %w", err)
	}

	if message.From != "" {
		if err := e.store.RemoveAddress(ctx, message.From); err != nil {
			return fmt.Errorf("learn spam: %w", err)
		}
	}

	return e.record(ctx, folder, message,
		judgement{verdict: VerdictSpam, reason: reason}, "train:spam", true)
}

func (e *Engine) record(
	ctx context.Context,
	folder string,
	message mail.Message,
	verdict judgement,
	action string,
	applied bool,
) error {
	e.log.InfoContext(ctx, "decision",
		"folder", folder, "uid", message.UID, "from", message.From, "subject", message.Subject,
		"verdict", verdict.verdict, "reason", verdict.reason, "score", verdict.score,
		"action", action, "applied", applied)

	_, err := e.store.RecordDecision(ctx, store.Decision{
		At:        time.Now(),
		Mailbox:   folder,
		UID:       message.UID,
		MessageID: message.MessageID,
		From:      message.From,
		Subject:   message.Subject,
		Score:     verdict.score,
		Verdict:   verdict.verdict,
		Reason:    verdict.reason,
		Action:    action,
		Applied:   applied,
	})
	if err != nil {
		return fmt.Errorf("record decision: %w", err)
	}

	return nil
}

// trainingKey identifies a message across folders: its Message-ID, or a
// hash of its content when it has none.
func trainingKey(message mail.Message) string {
	if message.MessageID != "" {
		return message.MessageID
	}

	sum := sha256.Sum256(message.Raw)

	return "sha256:" + hex.EncodeToString(sum[:])
}

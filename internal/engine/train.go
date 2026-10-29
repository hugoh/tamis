package engine

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/mail"
)

type trainFolder struct {
	name  string
	label classify.Label
	dest  string
}

// trainFolders: TrainSpam empties into Junk, TrainGood back into the first
// watched folder.
func (e *Engine) trainFolders() []trainFolder {
	return []trainFolder{
		{name: e.cfg.Folders.TrainSpam, label: classify.Spam, dest: e.cfg.Folders.Junk},
		{name: e.cfg.Folders.TrainGood, label: classify.Good, dest: e.cfg.Folders.Watch[0]},
	}
}

// train learns every message in a training folder, then moves it on. This
// is an explicit request from the user, so it runs in shadow mode too.
func (e *Engine) train(ctx context.Context, folder trainFolder, summary *Summary) error {
	if err := e.mail.Ensure(folder.name); err != nil {
		return fmt.Errorf("train from %s: %w", folder.name, err)
	}

	uids, err := e.mail.UIDsAfter(folder.name, 0)
	if err != nil {
		return fmt.Errorf("train from %s: %w", folder.name, err)
	}

	for batch := range slices.Chunk(uids, fetchBatch) {
		messages, err := e.mail.Fetch(folder.name, batch)
		if err != nil {
			return fmt.Errorf("train from %s: %w", folder.name, err)
		}

		for _, message := range messages {
			if err := e.trainOne(ctx, folder, message); err != nil {
				return err
			}

			summary.Trained++
		}

		if err := e.mail.AddKeyword(folder.name, batch, TrainedKeyword); err != nil {
			return fmt.Errorf("train from %s: %w", folder.name, err)
		}

		if err := e.mail.Move(folder.name, batch, folder.dest); err != nil {
			return fmt.Errorf("train from %s: %w", folder.name, err)
		}
	}

	return nil
}

func (e *Engine) trainOne(ctx context.Context, folder trainFolder, message mail.Message) error {
	tokens := classify.Tokenize(message.Raw)

	if folder.label == classify.Good {
		return e.learnGood(ctx, folder.name, message, tokens, ReasonTrainFolder)
	}

	if _, err := e.store.Train(ctx, trainingKey(message), tokens, classify.Spam); err != nil {
		return fmt.Errorf("train spam: %w", err)
	}

	return e.record(ctx, folder.name, message,
		judgement{verdict: VerdictSpam, reason: ReasonTrainFolder}, "train:spam", true)
}

// Source is a folder to learn from and how many of its most recent messages
// to take.
type Source struct {
	Folder string
	Limit  int
}

// BootstrapOptions says what to learn from.
type BootstrapOptions struct {
	// SentLimit is how many recent sent messages to whitelist recipients
	// from; zero skips the whitelist.
	SentLimit int
	// Spam folders are trained as spam.
	Spam []Source
	// Good folders are trained as good, but only messages that are read and
	// at least GoodMinAge old: unread or recent mail may be spam nobody has
	// looked at yet.
	Good       []Source
	GoodMinAge time.Duration
	// KeepCursors leaves the watched folders' cursors alone, so a bootstrap
	// of some folders doesn't skip mail that arrived unjudged in others.
	KeepCursors bool
}

// BootstrapSummary counts what Bootstrap learnt.
type BootstrapSummary struct {
	Addresses int
	Spam      int
	Good      int
	// AuthServerID is the provider's domain, when the sample named one.
	AuthServerID string
}

// Bootstrap seeds the whitelist from Sent and the model from the given
// folders, then starts watching from current mail.
func (e *Engine) Bootstrap(ctx context.Context, opts BootstrapOptions) (BootstrapSummary, error) {
	var (
		summary BootstrapSummary
		tally   auth.Tally
		err     error
	)

	if summary.Addresses, err = e.whitelistSent(ctx, opts.SentLimit); err != nil {
		return summary, err
	}

	for _, source := range opts.Spam {
		trained, err := e.trainSource(ctx, source, classify.Spam, &tally, func() ([]uint32, error) {
			return e.mail.UIDsAfter(source.Folder, 0)
		})

		summary.Spam += trained

		if err != nil {
			return summary, err
		}
	}

	for _, source := range opts.Good {
		trained, err := e.trainSource(ctx, source, classify.Good, &tally, func() ([]uint32, error) {
			return e.mail.SeenBefore(source.Folder, time.Now().Add(-opts.GoodMinAge))
		})

		summary.Good += trained

		if err != nil {
			return summary, err
		}
	}

	if summary.AuthServerID, err = e.learnAuthServerID(ctx, &tally); err != nil {
		return summary, err
	}

	if opts.KeepCursors {
		return summary, nil
	}

	return summary, e.startWatching(ctx)
}

// startWatching starts every watched folder from its current mail.
func (e *Engine) startWatching(ctx context.Context) error {
	for _, folder := range e.cfg.Folders.Watch {
		state, err := e.mail.State(folder)
		if err != nil {
			return fmt.Errorf("bootstrap %s: %w", folder, err)
		}

		if err := e.startAtCurrent(ctx, folder, state); err != nil {
			return err
		}
	}

	return nil
}

// trainSource trains the latest of the messages candidates returns.
func (e *Engine) trainSource(
	ctx context.Context,
	source Source,
	label classify.Label,
	tally *auth.Tally,
	candidates func() ([]uint32, error),
) (int, error) {
	uids, err := candidates()
	if err != nil {
		return 0, fmt.Errorf("bootstrap from %s: %w", source.Folder, err)
	}

	picked := latest(uids, source.Limit)
	e.log.InfoContext(ctx, "training",
		"folder", source.Folder, "label", label, "messages", len(picked))

	return e.trainCorpus(ctx, source.Folder, picked, label, tally)
}

func (e *Engine) whitelistSent(ctx context.Context, limit int) (int, error) {
	if limit == 0 {
		return 0, nil
	}

	uids, err := e.mail.UIDsAfter(e.cfg.Folders.Sent, 0)
	if err != nil {
		return 0, fmt.Errorf("bootstrap from %s: %w", e.cfg.Folders.Sent, err)
	}

	uids = latest(uids, limit)
	e.log.InfoContext(ctx, "whitelisting recipients",
		"folder", e.cfg.Folders.Sent, "messages", len(uids))

	recipients, err := e.mail.Recipients(e.cfg.Folders.Sent, uids)
	if err != nil {
		return 0, fmt.Errorf("bootstrap from %s: %w", e.cfg.Folders.Sent, err)
	}

	added := 0

	for _, address := range recipients {
		if strings.EqualFold(address, e.cfg.Account.User) {
			continue
		}

		if err := e.store.AddAddress(ctx, address, "sent"); err != nil {
			return added, fmt.Errorf("whitelist recipients: %w", err)
		}

		added++
	}

	return added, nil
}

func (e *Engine) trainCorpus(
	ctx context.Context,
	folder string,
	uids []uint32,
	label classify.Label,
	tally *auth.Tally,
) (int, error) {
	trained := 0

	for batch := range slices.Chunk(uids, fetchBatch) {
		messages, err := e.mail.Fetch(folder, batch)
		if err != nil {
			return trained, fmt.Errorf("bootstrap from %s: %w", folder, err)
		}

		for _, message := range messages {
			tally.Add(message.Raw)

			changed, err := e.store.Train(
				ctx,
				trainingKey(message),
				classify.Tokenize(message.Raw),
				label,
			)
			if err != nil {
				return trained, fmt.Errorf("bootstrap from %s: %w", folder, err)
			}

			if changed {
				trained++
			}
		}

		e.log.InfoContext(
			ctx,
			"training progress",
			"folder",
			folder,
			"done",
			trained,
			"total",
			len(uids),
		)
	}

	return trained, nil
}

// latest keeps the n highest (most recent) of ascending UIDs.
func latest(uids []uint32, n int) []uint32 {
	return uids[len(uids)-min(n, len(uids)):]
}

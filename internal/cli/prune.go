package cli

import (
	"context"
	"fmt"
	"log/slog"
	"time"

	"github.com/hugoh/tamis/internal/store"
)

const pruneEvery = 24 * time.Hour

// journalPruner deletes old decisions at most once a day. A new engine is
// built for every pass, so the run loop, which lives as long as the process,
// keeps the time.
type journalPruner struct {
	last time.Time
}

func (p *journalPruner) prune(
	ctx context.Context,
	database *store.Store,
	retention time.Duration,
	now time.Time,
	logger *slog.Logger,
) error {
	if retention <= 0 {
		return nil
	}

	if now.Sub(p.last) < pruneEvery {
		logger.DebugContext(ctx, "journal prune skipped", "next", p.last.Add(pruneEvery))

		return nil
	}

	cutoff := now.Add(-retention)

	pruned, err := database.PruneDecisions(ctx, cutoff)
	if err != nil {
		return fmt.Errorf("prune journal: %w", err)
	}

	p.last = now

	logger.DebugContext(ctx, "journal prune", "retention", retention, "cutoff", cutoff,
		"decisions", pruned)

	if pruned > 0 {
		logger.InfoContext(ctx, "journal pruned", "decisions", pruned)
	}

	return nil
}

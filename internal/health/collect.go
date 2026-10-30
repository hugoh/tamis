package health

import (
	"context"
	"net"
	"time"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/store"
)

// Collect gathers what the checks look at from the store and runs them.
func Collect(ctx context.Context, database *store.Store, cfg *config.Config) (*Report, error) {
	stats, err := database.Stats(ctx)
	if err != nil {
		return nil, err //nolint:wrapcheck // store errors carry their context
	}

	pin, err := engine.ResolveAuthPin(ctx, database, cfg)
	if err != nil {
		return nil, err //nolint:wrapcheck // store errors carry their context
	}

	recent, err := database.Decisions(ctx, Window)
	if err != nil {
		return nil, err //nolint:wrapcheck // store errors carry their context
	}

	web, err := collectWeb(ctx, database, cfg.Web)
	if err != nil {
		return nil, err
	}

	return Run(Input{
		Stats:     stats,
		Recent:    recent,
		Retention: cfg.Policy.JournalRetention.Duration,
		Auth:      pin,
		Web:       web,
		Now:       time.Now(),
	}), nil
}

const dialTimeout = time.Second

// collectWeb counts the passkeys and tries to connect to the listen address.
func collectWeb(ctx context.Context, database *store.Store, cfg config.Web) (Web, error) {
	web := Web{Enabled: cfg.Enabled, Listen: cfg.Listen, Origin: cfg.Origin}
	if !cfg.Enabled {
		return web, nil
	}

	keys, err := database.Passkeys(ctx)
	if err != nil {
		return Web{}, err //nolint:wrapcheck // store errors carry their context
	}

	web.Passkeys = len(keys)

	dialer := net.Dialer{Timeout: dialTimeout}
	if conn, err := dialer.DialContext(ctx, "tcp", cfg.Listen); err == nil {
		web.Reachable = true

		_ = conn.Close()
	}

	return web, nil
}

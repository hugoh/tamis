package engine

import (
	"context"
	"fmt"
	"strings"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/store"
)

// authSetting is the store key for the provider domain bootstrap learnt.
const authSetting = "auth_server_id"

// NoAuthServerID is the account.auth_server_id value that turns pinning off:
// only the topmost Authentication-Results header counts, whatever it names.
const NoAuthServerID = "none"

// authHostSetting is the store key for the IMAP host the learnt domain
// belongs to.
const authHostSetting = "auth_server_id_host"

// AuthSource says where the authserv-id tamis pins to came from.
type AuthSource string

// Where the authserv-id comes from.
const (
	AuthConfigured AuthSource = "configured"
	AuthLearnt     AuthSource = "learnt"
	AuthOff        AuthSource = "off"
	AuthUnset      AuthSource = "unset"
	// AuthStale means a domain was learnt for another IMAP host, so it is not
	// used until bootstrap learns it again.
	AuthStale AuthSource = "stale"
)

// AuthPin is the authserv-id tamis pins Authentication-Results to. An empty
// ID means pinning is off, which is safe: only the topmost header counts.
type AuthPin struct {
	ID     string
	Source AuthSource
	// LearntFor and Host are set when the pin is stale: the host the domain
	// was learnt for, and the one the account uses now.
	LearntFor string
	Host      string
}

// ResolveAuthPin works out the authserv-id to use: the configuration if it
// sets one, else what bootstrap learnt, provided it was learnt for the
// account's current host. A value learnt before hosts were recorded is used.
func ResolveAuthPin(
	ctx context.Context,
	database *store.Store,
	cfg *config.Config,
) (AuthPin, error) {
	switch configured := cfg.Account.AuthServerID; configured {
	case NoAuthServerID:
		return AuthPin{Source: AuthOff}, nil
	case "":
	default:
		return AuthPin{ID: configured, Source: AuthConfigured}, nil
	}

	learnt, found, err := database.Setting(ctx, authSetting)
	if err != nil || !found {
		return AuthPin{
			Source: AuthUnset,
		}, err //nolint:wrapcheck // store errors carry their context
	}

	learntFor, recorded, err := database.Setting(ctx, authHostSetting)
	if err != nil {
		return AuthPin{
			Source: AuthUnset,
		}, err //nolint:wrapcheck // store errors carry their context
	}

	if recorded && !strings.EqualFold(learntFor, cfg.Account.Host) {
		return AuthPin{Source: AuthStale, LearntFor: learntFor, Host: cfg.Account.Host}, nil
	}

	return AuthPin{ID: learnt, Source: AuthLearnt}, nil
}

// authServerID is the domain to pin Authentication-Results to, resolved once
// per engine.
func (e *Engine) authServerID(ctx context.Context) string {
	if e.authIDKnown {
		return e.authID
	}

	e.authIDKnown = true

	pin, err := ResolveAuthPin(ctx, e.store, e.cfg)
	if err != nil {
		e.log.WarnContext(ctx, "reading the learnt authserv-id failed; not pinning", "err", err)
	}

	if pin.Source == AuthStale {
		e.log.WarnContext(ctx, "the learnt authserv-id belongs to another host; not pinning. "+
			"Run tamis bootstrap to learn it again",
			"learnt_for", pin.LearntFor, "host", pin.Host)
	}

	e.authID = pin.ID

	return e.authID
}

// learnAuthServerID stores the provider's domain when the sample names one,
// and returns it. A sample that can't tell leaves what was learnt before.
func (e *Engine) learnAuthServerID(ctx context.Context, tally *auth.Tally) (string, error) {
	domain, ok := tally.Infer()
	if !ok {
		return "", nil
	}

	// The domain first: if the second write fails, the value is stale, which
	// is safe, and not wrongly trusted for a new host.
	if err := e.store.SetSetting(ctx, authSetting, domain); err != nil {
		return "", fmt.Errorf("learn authserv-id: %w", err)
	}

	if err := e.store.SetSetting(ctx, authHostSetting, e.cfg.Account.Host); err != nil {
		return "", fmt.Errorf("learn authserv-id: %w", err)
	}

	e.authID, e.authIDKnown = "", false

	e.log.InfoContext(ctx, "learnt the provider's authserv-id",
		"domain", domain, "sample", tally.Total())

	return domain, nil
}

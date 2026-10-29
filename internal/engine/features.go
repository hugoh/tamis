package engine

import (
	"context"
	"fmt"
	"slices"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/store"
)

// features tokenizes a message, adds the recipient status, and returns a
// model to score the tokens with. The status depends on which addresses good
// mail has gone to, so it comes from the model; asking for its three possible
// tokens up front keeps this to one lookup.
func (e *Engine) features(ctx context.Context, raw []byte) ([]string, *store.Snapshot, error) {
	tokens := classify.Tokenize(raw)

	model, err := e.store.Model(
		ctx,
		append(slices.Clone(tokens), classify.RecipientStatusTokens()...),
	)
	if err != nil {
		return nil, nil, fmt.Errorf("features: %w", err)
	}

	return classify.WithRecipientStatus(tokens, model, []string{e.cfg.Account.User}), model, nil
}

// tokens is features without the model, for training.
func (e *Engine) tokens(ctx context.Context, raw []byte) ([]string, error) {
	tokens, _, err := e.features(ctx, raw)

	return tokens, err
}

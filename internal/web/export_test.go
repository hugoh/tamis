package web

import (
	"io/fs"
	"log/slog"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/store"
)

// MissingAssets is missingAssets, for tests.
func MissingAssets(fsys fs.FS) []string { return missingAssets(fsys) }

// NewWithAssets is New with the embedded files replaced, for tests.
func NewWithAssets(
	cfg *config.Config,
	database *store.Store,
	logger *slog.Logger,
	opts Options,
	fsys fs.FS,
) (*Server, error) {
	return newServer(cfg, database, logger, opts, fsys)
}

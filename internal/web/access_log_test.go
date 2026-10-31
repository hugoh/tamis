package web_test

import (
	"bytes"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"

	"github.com/hugoh/tamis/internal/store"
	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

func TestRequestsAreLoggedAtDebugLevel(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		method string
		path   string
	}{
		{"public page", http.MethodGet, loginPath},
		{"page behind login", http.MethodGet, "/health"},
		{"refused request", http.MethodPost, "/rules/test"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
			require.NoError(t, err)
			t.Cleanup(func() { _ = db.Close() })

			var out bytes.Buffer

			logger := slog.New(
				slog.NewTextHandler(&out, &slog.HandlerOptions{Level: slog.LevelDebug}),
			)
			srv, err := web.New(testConfig(), db, logger, web.Options{})
			require.NoError(t, err)

			req := httptest.NewRequestWithContext(t.Context(), tt.method, tt.path, nil)
			rec := httptest.NewRecorder()
			srv.ServeHTTP(rec, req)

			line := out.String()
			require.Contains(t, line, "level=DEBUG")
			require.Contains(t, line, "method="+tt.method)
			require.Contains(t, line, "path="+tt.path)
			require.Contains(t, line, fmt.Sprintf("status=%d", rec.Code))
		})
	}
}

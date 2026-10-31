package web_test

import (
	"log/slog"
	"os"
	"testing"
	"testing/fstest"

	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const (
	themeCSS = "static/vendor/theme.css"
	htmxJS   = "static/vendor/htmx.min.js"
)

func TestMissingAssets(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		files fstest.MapFS
		want  []string
	}{
		"all there": {
			fstest.MapFS{
				themeCSS: {},
				htmxJS:   {},
			},
			nil,
		},
		"no stylesheet": {
			fstest.MapFS{htmxJS: {}},
			[]string{themeCSS},
		},
		"nothing": {
			fstest.MapFS{},
			[]string{themeCSS, htmxJS},
		},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			require.Equal(t, tt.want, web.MissingAssets(tt.files))
		})
	}
}

func TestHasAssetsReportsWhatWasBuiltIn(t *testing.T) {
	t.Parallel()

	_, cssErr := os.Stat(themeCSS)
	_, jsErr := os.Stat(htmxJS)

	require.Equal(
		t,
		cssErr == nil && jsErr == nil,
		web.HasAssets(),
		"embedded exactly when they were on disk at build",
	)
}

func TestNewRefusesWithoutTheFrontEndFiles(t *testing.T) {
	t.Parallel()

	_, err := web.NewWithAssets(
		testConfig(),
		newHarness(t).db,
		slog.New(slog.DiscardHandler),
		web.Options{},
		fstest.MapFS{},
	)
	require.ErrorContains(t, err, "theme.css")
	require.ErrorContains(t, err, "mise run vendor-web", "it says how to fix it")
}

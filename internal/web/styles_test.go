package web_test

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

// classEscape writes a class name the way a stylesheet selector does.
var classEscape = regexp.MustCompile(`[^A-Za-z0-9_-]`)

// TestStylesheetHasEveryClassTheTemplatesUse catches a template that gained a
// class without `mise run vendor-web` rebuilding theme.css, which would leave
// it unstyled.
func TestStylesheetHasEveryClassTheTemplatesUse(t *testing.T) {
	t.Parallel()

	css, err := os.ReadFile("static/vendor/theme.css")
	require.NoError(t, err)

	templates, err := filepath.Glob("templates/*.html")
	require.NoError(t, err)
	require.NotEmpty(t, templates)

	attribute := regexp.MustCompile(`class="([^"]*)"`)
	action := regexp.MustCompile(`(?s)\{\{.*?\}\}`)

	for _, path := range templates {
		source, err := os.ReadFile(path) //nolint:gosec // our own templates
		require.NoError(t, err)

		// Classes picked inside {{if}} stay; the action syntax goes, quotes and all.
		plain := action.ReplaceAllString(string(source), " ")

		for _, match := range attribute.FindAllStringSubmatch(plain, -1) {
			for class := range strings.FieldsSeq(match[1]) {
				selector := "." + classEscape.ReplaceAllStringFunc(
					class,
					func(char string) string { return `\` + char },
				)
				require.Contains(
					t,
					string(css),
					selector,
					"%s uses %q, which theme.css lacks: run mise run vendor-web",
					path,
					class,
				)
			}
		}
	}
}

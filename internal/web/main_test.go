package web_test

import (
	"fmt"
	"os"
	"testing"

	"github.com/hugoh/tamis/internal/web"
)

// The tests drive the real pages, which need the front-end files the build
// generates. Without them every test would fail the same way, so say it once.
func TestMain(m *testing.M) {
	if !web.HasAssets() {
		fmt.Fprintln(os.Stderr, "the web interface's front-end files are missing: "+
			"run mise run vendor-web first")
		os.Exit(1)
	}

	os.Exit(m.Run())
}

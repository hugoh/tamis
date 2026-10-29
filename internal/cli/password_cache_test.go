package cli

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestPasswordCommandRunsOnce(t *testing.T) {
	t.Parallel()

	runs := filepath.Join(t.TempDir(), "runs")
	finder := &passwordFinder{}
	who := credentials{command: []string{"sh", "-c", "echo run >> " + runs + "; echo secret"}}

	for range 3 {
		password, err := finder.find(t.Context(), who)
		require.NoError(t, err)
		require.Equal(t, "secret", password)
	}

	log, err := os.ReadFile(runs) //nolint:gosec // a path in the test's own temp dir
	require.NoError(t, err)
	require.Equal(t, 1, strings.Count(string(log), "run"))
}

func TestFailedPasswordCommandIsRetried(t *testing.T) {
	t.Parallel()

	flag := filepath.Join(t.TempDir(), "ready")
	finder := &passwordFinder{}
	who := credentials{command: []string{"sh", "-c", "test -f " + flag + " && echo secret"}}

	_, err := finder.find(t.Context(), who)
	require.Error(t, err)

	require.NoError(t, os.WriteFile(flag, nil, 0o600))

	password, err := finder.find(t.Context(), who)
	require.NoError(t, err)
	require.Equal(t, "secret", password)
}

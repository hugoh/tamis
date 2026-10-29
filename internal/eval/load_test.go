package eval_test

import (
	"os"
	"path/filepath"
	"regexp"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/eval"
	"github.com/stretchr/testify/require"
)

func write(t *testing.T, dir, name, subject string, at time.Time) {
	t.Helper()

	path := filepath.Join(dir, name)
	require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o700))
	require.NoError(
		t,
		os.WriteFile(path, mail("x@y.example", "me@example.com", subject, "body", at), 0o600),
	)
}

var subject = regexp.MustCompile(`Subject: (\S+)`)

func subjects(messages []eval.Message) []string {
	out := make([]string, len(messages))
	for i, m := range messages {
		out[i] = subject.FindStringSubmatch(string(m.Raw))[1]
	}

	return out
}

func TestLoadOrdersByDateAndSpreadsTheSpam(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	day := func(n int) time.Time { return time.Date(2026, 1, n, 0, 0, 0, 0, time.UTC) }

	// Written out of order, and in nested folders.
	write(t, filepath.Join(root, "ham"), "b/h3.eml", "h3", day(3))
	write(t, filepath.Join(root, "ham"), "a/h1.eml", "h1", day(1))
	write(t, filepath.Join(root, "ham"), "a/h4.eml", "h4", day(4))
	write(t, filepath.Join(root, "ham"), "h2.eml", "h2", day(2))
	write(t, filepath.Join(root, "spam"), "s2.eml", "s2", day(20))
	write(t, filepath.Join(root, "spam"), "s1.eml", "s1", day(10))

	messages, err := eval.Load(root)
	require.NoError(t, err)
	require.Equal(t, []string{"h1", "s1", "h2", "s2", "h3", "h4"}, subjects(messages))

	spam := 0

	for _, m := range messages {
		if m.Spam {
			spam++
		}
	}

	require.Equal(t, 2, spam)
}

func TestLoadNeedsBothFolders(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	write(t, filepath.Join(root, "ham"), "h.eml", "h", time.Now())

	_, err := eval.Load(root)
	require.ErrorIs(t, err, eval.ErrNoMessages)
}

package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/require"
)

const runCmd = "run"

func message(from, subject, body string, at time.Time) []byte {
	return fmt.Appendf(
		nil,
		"From: %s\r\nTo: me@example.com\r\nSubject: %s\r\nDate: %s\r\nMessage-ID: <%d@test>\r\n\r\n%s\r\n",
		from,
		subject,
		at.Format(time.RFC1123Z),
		at.UnixNano(),
		body,
	)
}

func TestParseSpec(t *testing.T) {
	t.Parallel()

	tests := []struct {
		spec    string
		folder  string
		limit   int
		wantErr bool
	}{
		{"Junk=1000", junkFolder, 1000, false},
		{"work/some folder=5", "work/some folder", 5, false},
		{"Junk", "", 0, true},
		{"Junk=", "", 0, true},
		{"Junk=lots", "", 0, true},
		{"Junk=0", "", 0, true},
		{"=5", "", 0, true},
	}

	for _, tt := range tests {
		t.Run(tt.spec, func(t *testing.T) {
			t.Parallel()

			folder, limit, err := parseSpec(tt.spec)
			if tt.wantErr {
				require.Error(t, err)

				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.folder, folder)
			require.Equal(t, tt.limit, limit)
		})
	}
}

func TestFetchFolderWritesTheLatestMessages(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr: srv.Addr, Username: imaptest.Username, Password: imaptest.Password, Plaintext: true,
	})
	require.NoError(t, err)
	t.Cleanup(func() { _ = client.Close() })

	for i := range 5 {
		_, err := client.Append(
			"Junk",
			message("s@spam.example", fmt.Sprintf("s%d", i), "body", time.Now()),
			nil,
			time.Time{},
		)
		require.NoError(t, err)
	}

	dir := filepath.Join(t.TempDir(), "spam")

	written, err := fetchFolder(client, "Junk", 3, dir)
	require.NoError(t, err)
	require.Equal(t, 3, written, "the latest three")

	entries, err := os.ReadDir(dir)
	require.NoError(t, err)
	require.Len(t, entries, 3)

	info, err := os.Stat(dir)
	require.NoError(t, err)
	require.Equal(t, os.FileMode(0o700), info.Mode().Perm(), "mail is for its owner only")

	file, err := entries[0].Info()
	require.NoError(t, err)
	require.Equal(t, os.FileMode(0o600), file.Mode().Perm())

	// Fetching reads without marking anything as seen.
	got, err := client.Fetch("Junk", []uint32{1})
	require.NoError(t, err)
	require.NotContains(t, got[0].Flags, `\seen`)
}

func TestRefusesToWriteMailInsideARepository(t *testing.T) {
	t.Parallel()

	repo := t.TempDir()
	require.NoError(t, os.Mkdir(filepath.Join(repo, ".jj"), 0o700))

	require.Error(t, checkOutsideRepo(filepath.Join(repo, "eval-data", "ham")))
	require.NoError(t, checkOutsideRepo(filepath.Join(t.TempDir(), "ham")))
}

func TestRunComparesVariants(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)

	for i := range 60 {
		at := start.Add(time.Duration(i) * time.Hour)

		path := filepath.Join(root, "ham", fmt.Sprintf("h%d.eml", i))
		body := "Agenda for the project meeting with the team"

		if i%3 == 0 {
			path = filepath.Join(root, "spam", fmt.Sprintf("s%d.eml", i))
			body = "Congratulations winner, claim your free lottery prize"
		}

		require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o700))
		require.NoError(t, os.WriteFile(path, message("x@y.example", "hello", body, at), 0o600))
	}

	var out, errOut bytes.Buffer

	err := run([]string{runCmd, "--warmup", "10", "--runs", "3", root}, &out, &errOut)
	require.NoError(t, err, errOut.String())
	require.Contains(t, out.String(), "baseline")
	require.Contains(t, out.String(), "recipient tokens")
	require.Contains(t, out.String(), "better in")
	require.Contains(t, out.String(), "40 good and 20 spam")
}

func TestRunUsage(t *testing.T) {
	t.Parallel()

	var out, errOut bytes.Buffer

	require.Error(t, run(nil, &out, &errOut))
	require.Error(t, run([]string{"bogus"}, &out, &errOut))
	require.Error(t, run([]string{runCmd}, &out, &errOut), "run needs a folder")
}

func TestRunCommandRefusals(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		args []string
	}{
		{"bad flag", []string{runCmd, "--bogus"}},
		{"no folder", []string{runCmd, "--own", "me@example.com"}},
		{"empty corpus folder", []string{runCmd, filepath.Join(t.TempDir(), "absent")}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			require.Error(t, run(tt.args, &bytes.Buffer{}, &bytes.Buffer{}))
		})
	}
}

func TestOwnAddressesSplitsTheFlag(t *testing.T) {
	t.Parallel()

	require.Equal(
		t,
		[]string{"a@x.example", "b@x.example"},
		ownAddresses("a@x.example,b@x.example"),
	)
}

func TestOwnAddressesDefaultsToTheAccount(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "tamis", "config.toml")
	require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o700))
	require.NoError(
		t,
		os.WriteFile(path, []byte("[account]\nhost = \"h\"\nuser = \"me@example.com\"\n"), 0o600),
	)
	t.Setenv("XDG_CONFIG_HOME", dir)

	require.Equal(t, []string{"me@example.com"}, ownAddresses(""))

	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	require.Nil(t, ownAddresses(""), "no config, no addresses")
}

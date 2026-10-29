package main

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/require"
)

const (
	junkFolder = "Junk"
	junkSpec   = "Junk=1"
	hamFlag    = "--ham"
	spamFlag   = "--spam"
	hamSpec    = "INBOX=1"
)

var errFake = errors.New("fake failure")

type fakeSource struct {
	uids     []uint32
	listErr  error
	fetchErr error
	fetched  [][]uint32
}

func (f *fakeSource) UIDsAfter(string, uint32) ([]uint32, error) {
	return f.uids, f.listErr
}

func (f *fakeSource) Fetch(_ string, uids []uint32) ([]mail.Message, error) {
	if f.fetchErr != nil {
		return nil, f.fetchErr
	}

	f.fetched = append(f.fetched, uids)

	messages := make([]mail.Message, 0, len(uids))
	for _, uid := range uids {
		messages = append(messages, mail.Message{UID: uid, Raw: []byte("raw")})
	}

	return messages, nil
}

func uidRange(n int) []uint32 {
	uids := make([]uint32, n)
	for i := range uids {
		uids[i] = uint32(i + 1)
	}

	return uids
}

func TestSpecsCollectRepeatedFlags(t *testing.T) {
	t.Parallel()

	var s specs

	require.NoError(t, s.Set("Junk=5"))
	require.NoError(t, s.Set("Spam=7"))
	require.Equal(t, "Junk=5,Spam=7", s.String())
}

func TestFetchFolder(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name        string
		source      *fakeSource
		limit       int
		prepare     func(t *testing.T, dir string)
		wantWritten int
		wantBatches []int
		wantErr     string
	}{
		{
			name:        "fewer messages than the limit",
			source:      &fakeSource{uids: uidRange(3)},
			limit:       10,
			wantWritten: 3,
			wantBatches: []int{3},
		},
		{
			name:        "latest messages only, in batches",
			source:      &fakeSource{uids: uidRange(130)},
			limit:       120,
			wantWritten: 120,
			wantBatches: []int{50, 50, 20},
		},
		{
			name:    "list fails",
			source:  &fakeSource{listErr: errFake},
			limit:   1,
			wantErr: "list Junk",
		},
		{
			name:    "fetch fails",
			source:  &fakeSource{uids: uidRange(2), fetchErr: errFake},
			limit:   2,
			wantErr: "fetch from Junk",
		},
		{
			name:   "target is not creatable",
			source: &fakeSource{uids: uidRange(1)},
			limit:  1,
			prepare: func(t *testing.T, dir string) {
				t.Helper()
				require.NoError(t, os.WriteFile(filepath.Dir(dir), nil, fileMode))
			},
			wantErr: "create",
		},
		{
			name:   "message file is not writable",
			source: &fakeSource{uids: uidRange(1)},
			limit:  1,
			prepare: func(t *testing.T, dir string) {
				t.Helper()
				require.NoError(t, os.MkdirAll(filepath.Join(dir, "00000001.eml"), dirMode))
			},
			wantErr: "write",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			parent := t.TempDir()
			dir := filepath.Join(parent, "target", "Junk")

			if tt.prepare != nil {
				tt.prepare(t, dir)
			}

			written, err := fetchFolder(tt.source, junkFolder, tt.limit, dir)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)

				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.wantWritten, written)

			sizes := make([]int, len(tt.source.fetched))
			for i, batch := range tt.source.fetched {
				sizes[i] = len(batch)
			}

			require.Equal(t, tt.wantBatches, sizes)
		})
	}
}

func TestFetchClass(t *testing.T) {
	t.Parallel()

	t.Run("writes each folder below its own directory", func(t *testing.T) {
		t.Parallel()

		dir := t.TempDir()

		var out bytes.Buffer

		err := fetchClass(&fakeSource{uids: uidRange(2)}, dir, specs{"work/some folder=2"}, &out)
		require.NoError(t, err)
		require.FileExists(t, filepath.Join(dir, "work_some_folder", "00000002.eml"))
		require.Contains(t, out.String(), "wrote 2 messages")
	})

	t.Run("bad spec", func(t *testing.T) {
		t.Parallel()

		err := fetchClass(&fakeSource{}, t.TempDir(), specs{junkFolder}, &bytes.Buffer{})
		require.ErrorIs(t, err, errSpec)
	})

	t.Run("fetch error", func(t *testing.T) {
		t.Parallel()

		err := fetchClass(
			&fakeSource{listErr: errFake},
			t.TempDir(),
			specs{junkSpec},
			&bytes.Buffer{},
		)
		require.ErrorIs(t, err, errFake)
	})
}

func TestFetchCommandRefusals(t *testing.T) {
	t.Parallel()

	repo := t.TempDir()
	require.NoError(t, os.Mkdir(filepath.Join(repo, ".git"), dirMode))

	missingConfig := filepath.Join(t.TempDir(), "absent.toml")

	tests := []struct {
		name    string
		args    []string
		wantErr error
	}{
		{"bad flag", []string{"--bogus"}, nil},
		{"no folder", []string{spamFlag, junkSpec, hamFlag, hamSpec}, errUsage},
		{"no spam", []string{hamFlag, hamSpec, t.TempDir()}, errUsage},
		{"no ham", []string{spamFlag, junkSpec, t.TempDir()}, errUsage},
		{
			"inside a repository",
			[]string{spamFlag, junkSpec, hamFlag, hamSpec, filepath.Join(repo, "data")},
			errInRepo,
		},
		{
			"unreadable config",
			[]string{
				"--config",
				missingConfig,
				spamFlag,
				junkSpec,
				hamFlag,
				hamSpec,
				t.TempDir(),
			},
			nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			err := fetchCommand(tt.args, &bytes.Buffer{}, &bytes.Buffer{})
			require.Error(t, err)

			if tt.wantErr != nil {
				require.ErrorIs(t, err, tt.wantErr)
			}
		})
	}
}

func TestConnectFailsWhenServerIsUnreachable(t *testing.T) {
	path := filepath.Join(t.TempDir(), "config.toml")
	body := "[account]\nhost = \"127.0.0.1\"\nport = 1\nuser = \"me\"\n"
	require.NoError(t, os.WriteFile(path, []byte(body), fileMode))
	t.Setenv(passwordEnv, "secret")

	client, err := connect(t.Context(), path)
	require.Error(t, err)
	require.Nil(t, client)
	require.ErrorContains(t, err, "fetch:")
}

func TestConnectNeedsAPassword(t *testing.T) {
	path := filepath.Join(t.TempDir(), "config.toml")
	body := "[account]\nhost = \"tamis-test.invalid\"\nport = 1\nuser = \"nobody\"\n"
	require.NoError(t, os.WriteFile(path, []byte(body), fileMode))
	t.Setenv(passwordEnv, "")

	_, err := connect(t.Context(), path)
	require.ErrorContains(t, err, "TAMIS_PASSWORD")
}

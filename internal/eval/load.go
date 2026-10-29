package eval

import (
	"bytes"
	"cmp"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/mail"
	"os"
	"path/filepath"
	"slices"
	"time"

	tamismail "github.com/hugoh/tamis/internal/mail"
)

// ErrNoMessages means a folder had no mail in it.
var ErrNoMessages = errors.New("no messages")

type dated struct {
	at  time.Time
	raw []byte
}

// Load reads root/ham and root/spam: any files below them are messages. Each
// class is ordered by its Date headers, and the spam is spread evenly through
// the good mail's time line, as if it had arrived all along. Real junk
// folders are purged, so their dates don't overlap an old archive's.
func Load(root string) ([]Message, error) {
	ham, err := readFolder(filepath.Join(root, "ham"))
	if err != nil {
		return nil, err
	}

	spam, err := readFolder(filepath.Join(root, "spam"))
	if err != nil {
		return nil, err
	}

	return interleave(ham, spam), nil
}

func readFolder(dir string) ([]dated, error) {
	var out []dated

	err := filepath.WalkDir(dir, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		if entry.IsDir() {
			return nil
		}

		file, err := os.Open(path) //nolint:gosec // the user names the folder to read
		if err != nil {
			return fmt.Errorf("open %s: %w", path, err)
		}

		defer func() { _ = file.Close() }()

		raw, err := io.ReadAll(io.LimitReader(file, tamismail.MaxMessageBytes))
		if err != nil {
			return fmt.Errorf("read %s: %w", path, err)
		}

		out = append(out, dated{at: sent(raw), raw: raw})

		return nil
	})
	if errors.Is(err, fs.ErrNotExist) {
		err = ErrNoMessages
	}

	if err != nil {
		return nil, fmt.Errorf("load %s: %w", dir, err)
	}

	if len(out) == 0 {
		return nil, fmt.Errorf("load %s: %w", dir, ErrNoMessages)
	}

	slices.SortStableFunc(
		out,
		func(a, b dated) int { return cmp.Compare(a.at.UnixNano(), b.at.UnixNano()) },
	)

	return out, nil
}

// sent is the message's Date header, or the zero time.
func sent(raw []byte) time.Time {
	parsed, err := mail.ReadMessage(bytes.NewReader(raw))
	if err != nil {
		return time.Time{}
	}

	at, err := parsed.Header.Date()
	if err != nil {
		return time.Time{}
	}

	return at
}

func interleave(ham, spam []dated) []Message {
	out := make([]Message, 0, len(ham)+len(spam))
	next := 0

	for i, message := range ham {
		out = append(out, Message{Raw: message.raw})

		for next < len(spam) && next*len(ham) <= (i+1)*len(spam) {
			out = append(out, Message{Raw: spam[next].raw, Spam: true})
			next++
		}
	}

	for ; next < len(spam); next++ {
		out = append(out, Message{Raw: spam[next].raw, Spam: true})
	}

	return out
}

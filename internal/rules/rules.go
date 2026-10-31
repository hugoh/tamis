// Package rules files mail into folders by running a Sieve script (RFC 5228)
// against it, the way a server-side filter would.
package rules

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/mail"
	"net/textproto"
	"os"
	"strings"

	"github.com/foxcpp/go-sieve"
	"github.com/foxcpp/go-sieve/interp"
)

// Rules is a loaded Sieve script.
type Rules struct {
	script *sieve.Script
}

// Load reads and compiles the script at path.
func Load(path string) (*Rules, error) {
	source, err := os.ReadFile(path) //nolint:gosec // the path is the user's own rules file
	if err != nil {
		return nil, fmt.Errorf("read rules: %w", err)
	}

	return Parse(bytes.NewReader(source))
}

// Parse compiles a script, so a mistake surfaces at startup and not mid-pass.
func Parse(r io.Reader) (*Rules, error) {
	script, err := sieve.Load(bufio.NewReader(r), sieve.DefaultOptions())
	if err != nil {
		return nil, fmt.Errorf("parse rules: %w", err)
	}

	return &Rules{script: script}, nil
}

// Folder runs the script on a raw message and returns the folder its first
// fileinto names, or "" when the message stays where it is. Redirects and
// every other action are ignored: only filing makes sense over IMAP.
func (r *Rules) Folder(ctx context.Context, raw []byte, from string) (string, error) {
	header, err := textproto.NewReader(bufio.NewReader(bytes.NewReader(raw))).ReadMIMEHeader()
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("read headers: %w", err)
	}

	data := sieve.NewRuntimeData(
		r.script,
		denyRedirects{},
		interp.EnvelopeStatic{From: from},
		interp.MessageStatic{Size: len(raw), Header: header, RawMessage: raw},
	)

	if err := r.script.Execute(ctx, data); err != nil {
		return "", fmt.Errorf("run rules: %w", err)
	}

	for _, action := range data.AppliedActions {
		if file, ok := action.(interp.ActionFileInto); ok && !file.Copy {
			return file.Mailbox, nil
		}
	}

	return "", nil
}

type denyRedirects struct{}

func (denyRedirects) RedirectAllowed(context.Context, *interp.RuntimeData, string) (bool, error) {
	return false, nil
}

// SenderOf is the lower-cased address in a raw message's From header, or ""
// when it has none.
func SenderOf(raw []byte) string {
	message, err := mail.ReadMessage(bytes.NewReader(raw))
	if err != nil {
		return ""
	}

	addr, err := mail.ParseAddress(message.Header.Get("From"))
	if err != nil {
		return ""
	}

	return strings.ToLower(addr.Address)
}

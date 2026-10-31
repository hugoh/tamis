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
	"maps"
	"net/mail"
	"net/textproto"
	"os"
	"slices"
	"strings"

	"github.com/foxcpp/go-sieve"
	"github.com/foxcpp/go-sieve/interp"
	"github.com/foxcpp/go-sieve/lexer"
	"github.com/foxcpp/go-sieve/parser"
)

// Rules is a loaded Sieve script.
type Rules struct {
	script  *sieve.Script
	targets []string
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
	opts := sieve.DefaultOptions()

	tokens, err := lexer.Lex(bufio.NewReader(r), &opts.Lexer)
	if err != nil {
		return nil, fmt.Errorf("parse rules: %w", err)
	}

	commands, err := parser.Parse(lexer.NewStream(tokens), &opts.Parser)
	if err != nil {
		return nil, fmt.Errorf("parse rules: %w", err)
	}

	script, err := interp.LoadScript(commands, &opts.Interp)
	if err != nil {
		return nil, fmt.Errorf("parse rules: %w", err)
	}

	return &Rules{script: script, targets: fileintoTargets(commands)}, nil
}

// Targets are the folders the script can file into, sorted and without
// repeats. A name built from a variable can't be known before the script runs
// and is left out.
func (r *Rules) Targets() []string {
	return r.targets
}

func fileintoTargets(commands []parser.Cmd) []string {
	found := map[string]struct{}{}
	collectTargets(commands, found)

	if len(found) == 0 {
		return nil
	}

	return slices.Sorted(maps.Keys(found))
}

func collectTargets(commands []parser.Cmd, found map[string]struct{}) {
	for _, command := range commands {
		if command.Id == "fileinto" {
			for _, arg := range command.Args {
				if name, ok := arg.(parser.StringArg); ok && !strings.Contains(name.Value, "${") {
					found[name.Value] = struct{}{}
				}
			}
		}

		collectTargets(command.Block, found)
	}
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

// Absent is a folder the script files into that the mailbox lacks, with the
// existing name closest to it when one is within a couple of typos.
type Absent struct {
	Folder, Nearest string
}

// maxTypos is how far a folder may be from a target and still be offered as
// its correction.
const maxTypos = 2

// Missing returns the targets that are not among existing, in order.
func Missing(targets, existing []string) []Absent {
	var absent []Absent

	for _, target := range targets {
		if !slices.Contains(existing, target) {
			absent = append(absent, Absent{Folder: target, Nearest: nearest(target, existing)})
		}
	}

	return absent
}

func nearest(target string, existing []string) string {
	best, bestDistance := "", maxTypos+1

	for _, name := range existing {
		if distance := editDistance(
			strings.ToLower(target),
			strings.ToLower(name),
		); distance < bestDistance {
			best, bestDistance = name, distance
		}
	}

	return best
}

func editDistance(left, right string) int {
	previous := make([]int, len(right)+1)
	for col := range previous {
		previous[col] = col
	}

	for row := 1; row <= len(left); row++ {
		current := make([]int, len(right)+1)
		current[0] = row

		for col := 1; col <= len(right); col++ {
			cost := 1
			if left[row-1] == right[col-1] {
				cost = 0
			}

			current[col] = min(previous[col]+1, current[col-1]+1, previous[col-1]+cost)
		}

		previous = current
	}

	return previous[len(right)]
}

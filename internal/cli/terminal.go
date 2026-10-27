package cli

import (
	"errors"
	"fmt"
	"io"
	"os"

	"golang.org/x/term"
)

// ErrNotTerminal means there is no terminal to prompt on.
var ErrNotTerminal = errors.New("not a terminal")

// TerminalPrompt returns an Env.Prompt that reads from in without echo,
// writing the prompt to out.
func TerminalPrompt(in *os.File, out io.Writer) func(string) (string, error) {
	return func(prompt string) (string, error) {
		descriptor := int(in.Fd())
		if !term.IsTerminal(descriptor) {
			return "", ErrNotTerminal
		}

		_, _ = fmt.Fprint(out, prompt)

		secret, err := term.ReadPassword(descriptor)

		_, _ = fmt.Fprintln(out)

		if err != nil {
			return "", fmt.Errorf("read from terminal: %w", err)
		}

		return string(secret), nil
	}
}

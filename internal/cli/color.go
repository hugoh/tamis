package cli

import (
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"unicode/utf8"

	"golang.org/x/term"
)

// style is an ANSI SGR code.
type style string

const (
	styleBold    style = "1"
	styleDim     style = "2"
	styleRed     style = "31"
	styleGreen   style = "32"
	styleYellow  style = "33"
	styleMagenta style = "35"
)

var errColorMode = errors.New("--color must be auto, always or never")

const (
	colorAuto   = "auto"
	colorAlways = "always"
	colorNever  = "never"
)

// palette paints text, or leaves it alone when colour is off.
type palette struct {
	on bool
}

func (p palette) paint(text string, s style) string {
	if !p.on || s == "" || text == "" {
		return text
	}

	return "\x1b[" + string(s) + "m" + text + "\x1b[0m"
}

// colorEnabled decides whether to colour output: always and never are
// explicit; auto colours a terminal unless NO_COLOR is set (no-color.org) or
// TERM is dumb.
func colorEnabled(mode string, isTerminal bool, getenv func(string) string) (bool, error) {
	switch mode {
	case colorAlways:
		return true, nil
	case colorNever:
		return false, nil
	case colorAuto, "":
		return isTerminal && getenv("NO_COLOR") == "" && getenv("TERM") != "dumb", nil
	default:
		return false, fmt.Errorf("%w, not %q", errColorMode, mode)
	}
}

// paletteFor builds the palette for out from the --color flag.
func (a *app) paletteFor(out io.Writer) (palette, error) {
	file, isFile := out.(*os.File)
	terminal := isFile && term.IsTerminal(int(file.Fd()))

	on, err := colorEnabled(a.color, terminal, a.env.Getenv)

	return palette{on: on}, err
}

// span is a run of text in one style; a cell is one or more spans.
type span struct {
	text  string
	style style
}

type cell []span

func (c cell) width() int {
	width := 0

	for _, part := range c {
		width += utf8.RuneCountInString(part.text)
	}

	return width
}

func (c cell) render(colors palette) string {
	var out strings.Builder

	for _, part := range c {
		out.WriteString(colors.paint(part.text, part.style))
	}

	return out.String()
}

const tableGap = 2

// writeTable prints rows as aligned columns. Columns are padded by the width
// of their visible text, so colour doesn't shift them; the last cell of a row
// isn't padded, so it doesn't count towards the width either (rows may differ
// in length, as a heading above a table does).
func writeTable(out io.Writer, colors palette, rows [][]cell) error {
	var widths []int

	for _, row := range rows {
		for i, c := range row[:max(len(row)-1, 0)] {
			if i == len(widths) {
				widths = append(widths, 0)
			}

			widths[i] = max(widths[i], c.width())
		}
	}

	var text strings.Builder

	for _, row := range rows {
		for i, c := range row {
			text.WriteString(c.render(colors))

			if i < len(row)-1 {
				text.WriteString(strings.Repeat(" ", widths[i]-c.width()+tableGap))
			}
		}

		text.WriteByte('\n')
	}

	if _, err := io.WriteString(out, text.String()); err != nil {
		return fmt.Errorf("write table: %w", err)
	}

	return nil
}

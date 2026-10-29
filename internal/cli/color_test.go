package cli

import (
	"bytes"
	"regexp"
	"testing"

	"github.com/stretchr/testify/require"
)

var ansi = regexp.MustCompile("\x1b\\[[0-9;]*m")

func plain(text string) string { return ansi.ReplaceAllString(text, "") }

func TestColorEnabled(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name     string
		mode     string
		terminal bool
		env      map[string]string
		want     bool
		wantErr  bool
	}{
		{"auto on a terminal", colorAuto, true, nil, true, false},
		{"auto when piped", colorAuto, false, nil, false, false},
		{"default is auto", "", true, nil, true, false},
		{
			"NO_COLOR wins in auto",
			colorAuto,
			true,
			map[string]string{"NO_COLOR": "1"},
			false,
			false,
		},
		{"dumb terminal", colorAuto, true, map[string]string{"TERM": "dumb"}, false, false},
		{"always, even piped", colorAlways, false, nil, true, false},
		{
			"always overrides NO_COLOR",
			colorAlways,
			false,
			map[string]string{"NO_COLOR": "1"},
			true,
			false,
		},
		{"never, even on a terminal", colorNever, true, nil, false, false},
		{"unknown mode", "sometimes", true, nil, false, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got, err := colorEnabled(
				tt.mode,
				tt.terminal,
				func(key string) string { return tt.env[key] },
			)
			if tt.wantErr {
				require.ErrorContains(t, err, "--color")

				return
			}

			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}

func TestWriteTableAlignsByVisibleWidth(t *testing.T) {
	t.Parallel()

	rows := [][]cell{
		{{{"ID", styleBold}}, {{"VERDICT", styleBold}}, {{"SUBJECT", styleBold}}},
		{{{"1", ""}}, {{"spam", styleRed}}, {{"héllo", ""}}},
		{{{"22", ""}}, {{"good", styleGreen}, {" (x)", styleDim}}, {{"bye", ""}}},
	}

	var off, on bytes.Buffer

	require.NoError(t, writeTable(&off, palette{}, rows))
	require.NoError(t, writeTable(&on, palette{on: true}, rows))

	require.NotContains(t, off.String(), "\x1b")
	require.Contains(t, on.String(), "\x1b[31mspam\x1b[0m")
	require.Equal(t, off.String(), plain(on.String()), "colour must not change the layout")
	require.Equal(t,
		"ID  VERDICT   SUBJECT\n1   spam      héllo\n22  good (x)  bye\n",
		off.String())
}

package report_test

import (
	"bytes"
	"fmt"
	"io"
	"testing"

	"github.com/hugoh/tamis/internal/report"
)

func TestWriteJSONIndents(t *testing.T) {
	var out bytes.Buffer

	if err := report.WriteJSON(&out, map[string]int{"a": 1}); err != nil {
		t.Fatal(err)
	}

	if want := "{\n  \"a\": 1\n}\n"; out.String() != want {
		t.Fatalf("got %q, want %q", out.String(), want)
	}
}

func TestWriteTableAligns(t *testing.T) {
	var out bytes.Buffer

	err := report.WriteTable(&out, func(table io.Writer) {
		_, _ = fmt.Fprint(table, "a\tb\nlonger\tc\n")
	})
	if err != nil {
		t.Fatal(err)
	}

	if want := "a       b\nlonger  c\n"; out.String() != want {
		t.Fatalf("got %q, want %q", out.String(), want)
	}
}

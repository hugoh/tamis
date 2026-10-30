package live

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"sync"
	"time"
)

// Entry is one log line.
type Entry struct {
	Time  time.Time
	Level slog.Level
	// Text is the message followed by its attributes as key=value.
	Text string
}

// Ring keeps the most recent log entries. A nil *Ring has none.
type Ring struct {
	mu      sync.Mutex
	entries []Entry
	next    int
	full    bool
}

// NewRing keeps up to size entries.
func NewRing(size int) *Ring {
	return &Ring{entries: make([]Entry, size)}
}

// Entries returns what the ring holds, newest first.
func (r *Ring) Entries() []Entry {
	if r == nil {
		return nil
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	count := r.next
	if r.full {
		count = len(r.entries)
	}

	out := make([]Entry, 0, count)
	for i := 1; i <= count; i++ {
		out = append(out, r.entries[(r.next-i+len(r.entries))%len(r.entries)])
	}

	return out
}

// Handler wraps next so that what it logs is also kept in the ring. The ring
// sees only what next would handle.
func (r *Ring) Handler(next slog.Handler) slog.Handler {
	if r == nil {
		return next
	}

	return &tee{ring: r, next: next}
}

func (r *Ring) add(entry Entry) {
	r.mu.Lock()
	defer r.mu.Unlock()

	r.entries[r.next] = entry

	r.next++
	if r.next == len(r.entries) {
		r.next, r.full = 0, true
	}
}

type tee struct {
	ring   *Ring
	next   slog.Handler
	prefix string
	attrs  []string
}

func (t *tee) Enabled(ctx context.Context, level slog.Level) bool {
	return t.next.Enabled(ctx, level)
}

func (t *tee) Handle(ctx context.Context, record slog.Record) error {
	var text strings.Builder

	text.WriteString(record.Message)

	for _, attr := range t.attrs {
		text.WriteString(" " + attr)
	}

	record.Attrs(func(attr slog.Attr) bool {
		text.WriteString(" " + format(t.prefix, attr))

		return true
	})

	at := record.Time
	if at.IsZero() {
		at = time.Now()
	}

	t.ring.add(Entry{Time: at, Level: record.Level, Text: text.String()})

	if err := t.next.Handle(ctx, record); err != nil {
		return fmt.Errorf("log: %w", err)
	}

	return nil
}

func (t *tee) WithAttrs(attrs []slog.Attr) slog.Handler {
	formatted := make([]string, len(t.attrs), len(t.attrs)+len(attrs))
	copy(formatted, t.attrs)

	for _, attr := range attrs {
		formatted = append(formatted, format(t.prefix, attr))
	}

	return &tee{ring: t.ring, next: t.next.WithAttrs(attrs), prefix: t.prefix, attrs: formatted}
}

func (t *tee) WithGroup(name string) slog.Handler {
	return &tee{
		ring:   t.ring,
		next:   t.next.WithGroup(name),
		prefix: t.prefix + name + ".",
		attrs:  t.attrs,
	}
}

func format(prefix string, attr slog.Attr) string {
	return prefix + attr.Key + "=" + attr.Value.Resolve().String()
}

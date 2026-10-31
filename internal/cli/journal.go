package cli

import (
	"context"
	"io"
	"log/slog"
	"sync"
)

// journalHandler logs for systemd's journal: each line starts with the
// priority marker the journal reads (sd-daemon(3)), so `journalctl -p warning`
// works, and carries no timestamp, since the journal adds its own.
type journalHandler struct {
	inner slog.Handler
	sink  *prioritySink
}

// prioritySink writes each record's line behind the marker for its level.
// The handler holds the lock from choosing the marker until the line is
// written, so a line never gets another record's priority.
type prioritySink struct {
	mu     sync.Mutex
	out    io.Writer
	marker string
}

func (s *prioritySink) Write(line []byte) (int, error) {
	if _, err := io.WriteString(s.out, s.marker); err != nil {
		return 0, err //nolint:wrapcheck // a failed write to stderr says enough
	}

	return s.out.Write(line) //nolint:wrapcheck // a failed write to stderr says enough
}

func newJournalHandler(out io.Writer, level slog.Level) *journalHandler {
	sink := &prioritySink{out: out}

	return &journalHandler{
		sink: sink,
		inner: slog.NewTextHandler(sink, &slog.HandlerOptions{
			Level: level,
			ReplaceAttr: func(groups []string, attr slog.Attr) slog.Attr {
				if len(groups) == 0 && attr.Key == slog.TimeKey {
					return slog.Attr{}
				}

				return attr
			},
		}),
	}
}

// journalMarker is the syslog priority for a level: err, warning, info, debug.
func journalMarker(level slog.Level) string {
	switch {
	case level >= slog.LevelError:
		return "<3>"
	case level >= slog.LevelWarn:
		return "<4>"
	case level >= slog.LevelInfo:
		return "<6>"
	default:
		return "<7>"
	}
}

func (h *journalHandler) Enabled(ctx context.Context, level slog.Level) bool {
	return h.inner.Enabled(ctx, level)
}

func (h *journalHandler) Handle(ctx context.Context, record slog.Record) error {
	h.sink.mu.Lock()
	defer h.sink.mu.Unlock()

	h.sink.marker = journalMarker(record.Level)

	return h.inner.Handle(ctx, record) //nolint:wrapcheck // slog handlers report their own errors
}

func (h *journalHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	return &journalHandler{inner: h.inner.WithAttrs(attrs), sink: h.sink}
}

func (h *journalHandler) WithGroup(name string) slog.Handler {
	return &journalHandler{inner: h.inner.WithGroup(name), sink: h.sink}
}

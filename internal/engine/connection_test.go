package engine_test

import (
	"bytes"
	"log/slog"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/require"
)

func onSent(op string) func(string, string) bool {
	return func(got, mailbox string) bool { return got == op && mailbox == sent }
}

func TestADroppedConnectionDuringTheSentScanFailsThePass(t *testing.T) {
	t.Parallel()

	e := setupServer(t, config.ModeShadow, imaptest.Options{
		Caps: imaptest.Rev2(), Drop: onSent("STATUS"),
	})
	e.append(
		t,
		sent,
		[]byte("From: me@example.com\r\nTo: a@example.com\r\nSubject: hi\r\n\r\nhello\r\n"),
	)

	var logs bytes.Buffer

	logged := engine.New(
		e.client, e.db, e.cfg, slog.New(slog.NewTextHandler(&logs, nil)),
	)

	_, err := logged.RunOnce(t.Context())
	require.Error(t, err)
	require.True(t, mail.IsConnectionError(err), "got: %v", err)
	require.NotContains(t, logs.String(), "not learning recipients", "it isn't about Sent")
}

func TestAFailureOnSentIsOnlyAWarning(t *testing.T) {
	t.Parallel()

	e := setupWith(t, config.ModeShadow, onSent("STATUS"))
	e.append(
		t,
		sent,
		[]byte("From: me@example.com\r\nTo: a@example.com\r\nSubject: hi\r\n\r\nhello\r\n"),
	)

	var logs bytes.Buffer

	logged := engine.New(
		e.client, e.db, e.cfg, slog.New(slog.NewTextHandler(&logs, nil)),
	)

	_, err := logged.RunOnce(t.Context())
	require.NoError(t, err, "the server said no; the connection is fine")
	require.Contains(t, logs.String(), "not learning recipients from Sent")
}

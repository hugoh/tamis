package mail_test

import (
	"bufio"
	"bytes"
	"context"
	"fmt"
	"log/slog"
	"net"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func connect(t *testing.T, srv *imaptest.Server) *mail.Client {
	t.Helper()

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr:      srv.Addr,
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		Plaintext: srv.TLSConfig == nil,
		TLSConfig: srv.TLSConfig,
	})
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, client.Close()) })

	return client
}

func raw(from, to, subject string) []byte {
	return fmt.Appendf(
		nil,
		"From: %s\r\nTo: %s\r\nCc: cc@example.org\r\nSubject: %s\r\nMessage-ID: <%s@test>\r\n\r\nbody\r\n",
		from,
		to,
		subject,
		subject,
	)
}

func seed(t *testing.T, client *mail.Client, mailbox string, messages ...[]byte) []uint32 {
	t.Helper()

	uids := make([]uint32, 0, len(messages))

	for _, message := range messages {
		uid, err := client.Append(mailbox, message, nil, time.Time{})
		require.NoError(t, err)

		uids = append(uids, uid)
	}

	return uids
}

func TestState(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	before, err := client.State("INBOX")
	require.NoError(t, err)
	require.NotZero(t, before.UIDValidity)

	uids := seed(t, client, "INBOX", raw("a@x", "b@y", "one"))

	after, err := client.State("INBOX")
	require.NoError(t, err)
	require.Equal(t, before.UIDValidity, after.UIDValidity)
	require.Equal(t, uids[0]+1, after.UIDNext)
}

func TestUIDsAfter(t *testing.T) {
	t.Parallel()

	// UIDs 1 and 2 are seeded into a fresh mailbox for each case.
	tests := []struct {
		name  string
		after uint32
		want  []uint32
	}{
		{"all", 0, []uint32{1, 2}},
		{"newer than first", 1, []uint32{2}},
		{"none newer", 2, nil},
		{"far beyond", 102, nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			client := connect(t, imaptest.Start(t, imaptest.Rev2()))
			seed(t, client, "INBOX", raw("a@x", "b@y", "one"), raw("a@x", "b@y", "two"))

			got, err := client.UIDsAfter("INBOX", tt.after)
			require.NoError(t, err)

			if len(tt.want) == 0 {
				require.Empty(t, got)

				return
			}

			require.Equal(t, tt.want, got)
		})
	}
}

func TestFetchPeeks(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))
	uids := seed(t, client, "INBOX", raw("alice@example.com", "me@example.com", "hello"))

	messages, err := client.Fetch("INBOX", uids)
	require.NoError(t, err)
	require.Len(t, messages, 1)
	require.Equal(t, uids[0], messages[0].UID)
	require.Contains(t, string(messages[0].Raw), "Subject: hello")
	require.Equal(t, "<hello@test>", messages[0].MessageID)
	require.Equal(t, "alice@example.com", messages[0].From)
	require.Equal(t, "hello", messages[0].Subject)
	require.NotContains(t, messages[0].Flags, `\seen`)

	again, err := client.Fetch("INBOX", uids)
	require.NoError(t, err)
	require.NotContains(t, again[0].Flags, `\seen`, "fetching must not mark mail as read")
}

func TestFetchEmpty(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	messages, err := client.Fetch("INBOX", nil)
	require.NoError(t, err)
	require.Empty(t, messages)
}

func TestSeenBefore(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))
	old := time.Now().AddDate(0, 0, -30)

	oldSeen, err := client.Append("INBOX", raw("a@x", "b@y", "oldseen"), []string{`\Seen`}, old)
	require.NoError(t, err)

	_, err = client.Append("INBOX", raw("a@x", "b@y", "oldunseen"), nil, old)
	require.NoError(t, err)

	_, err = client.Append("INBOX", raw("a@x", "b@y", "newseen"), []string{`\Seen`}, time.Now())
	require.NoError(t, err)

	got, err := client.SeenBefore("INBOX", time.Now().AddDate(0, 0, -7))
	require.NoError(t, err)
	require.Equal(t, []uint32{oldSeen}, got)
}

func TestRecipients(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))
	require.NoError(t, client.Ensure("Sent Messages"))

	uids := seed(t, client, "Sent Messages",
		raw("me@example.com", "Friend <Friend@Example.com>", "s1"),
		raw("me@example.com", "other@example.net", "s2"))

	got, err := client.Recipients("Sent Messages", uids)
	require.NoError(t, err)
	require.ElementsMatch(
		t,
		[]string{"friend@example.com", "other@example.net", "cc@example.org"},
		got,
	)
}

func TestMove(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		caps    imap.CapSet
		wantErr error
	}{
		{"native MOVE", imaptest.Rev2(), nil},
		{
			"COPY fallback with UIDPLUS",
			imap.CapSet{imap.CapIMAP4rev1: {}, imap.CapUIDPlus: {}},
			nil,
		},
		{"no MOVE or UIDPLUS", imaptest.Rev1(), mail.ErrUnsafeMove},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			client := connect(t, imaptest.Start(t, tt.caps))
			uids := seed(t, client, "INBOX", raw("a@x", "b@y", "m1"), raw("a@x", "b@y", "m2"))

			err := client.Move("INBOX", uids[:1], "Junk")
			if tt.wantErr != nil {
				require.ErrorIs(t, err, tt.wantErr)

				left, err := client.UIDsAfter("INBOX", 0)
				require.NoError(t, err)
				require.Equal(t, uids, left, "a refused move leaves the folder untouched")

				return
			}

			require.NoError(t, err)

			left, err := client.UIDsAfter("INBOX", 0)
			require.NoError(t, err)
			require.Equal(t, uids[1:], left)

			moved, err := client.UIDsAfter("Junk", 0)
			require.NoError(t, err)
			require.Len(t, moved, 1)
		})
	}
}

func TestAddKeyword(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))
	uids := seed(t, client, "INBOX", raw("a@x", "b@y", "k"))

	require.NoError(t, client.AddKeyword("INBOX", uids, "$TamisFiled"))

	messages, err := client.Fetch("INBOX", uids)
	require.NoError(t, err)
	require.Contains(t, messages[0].Flags, "$tamisfiled")
}

func TestRemoveKeyword(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))
	uids := seed(t, client, "INBOX", raw("a@x", "b@y", "k"))

	require.NoError(t, client.AddKeyword("INBOX", uids, "$TamisFiled"))
	require.NoError(t, client.RemoveKeyword("INBOX", uids, "$TamisFiled"))

	messages, err := client.Fetch("INBOX", uids)
	require.NoError(t, err)
	require.NotContains(t, messages[0].Flags, "$tamisfiled")
}

func TestEnsureIsIdempotent(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	require.NoError(t, client.Ensure("TrainSpam"))
	require.NoError(t, client.Ensure("TrainSpam"))
	require.NoError(t, client.Ensure("INBOX"))

	_, err := client.State("TrainSpam")
	require.NoError(t, err)
}

func TestDialTLS(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true}))

	_, err := client.State("INBOX")
	require.NoError(t, err)
}

func TestDialBadPassword(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())

	_, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr: srv.Addr, Username: imaptest.Username, Password: "wrong", Plaintext: true,
	})
	require.ErrorIs(t, err, mail.ErrLogin)
}

func TestMissingMailbox(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	_, err := client.State("Nope")
	require.Error(t, err)

	_, err = client.UIDsAfter("Nope", 0)
	require.Error(t, err)
}

func TestRecipientsEmpty(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	got, err := client.Recipients("INBOX", nil)
	require.NoError(t, err)
	require.Empty(t, got)

	_, err = client.State("INBOX")
	require.NoError(t, err, "the connection survives")
}

func TestServerFailures(t *testing.T) {
	t.Parallel()

	tests := []struct {
		op    string
		check func(t *testing.T, client *mail.Client, uids []uint32)
	}{
		{"FETCH", func(t *testing.T, client *mail.Client, uids []uint32) {
			t.Helper()

			_, err := client.Fetch("INBOX", uids)
			require.Error(t, err)
		}},
		{"MOVE", func(t *testing.T, client *mail.Client, uids []uint32) {
			t.Helper()
			require.Error(t, client.Move("INBOX", uids, "Junk"))
		}},
		{"STORE", func(t *testing.T, client *mail.Client, uids []uint32) {
			t.Helper()
			require.Error(t, client.AddKeyword("INBOX", uids, "$X"))
		}},
		{"STORE", func(t *testing.T, client *mail.Client, uids []uint32) {
			t.Helper()
			require.Error(t, client.RemoveKeyword("INBOX", uids, "$X"))
		}},
		{"APPEND", func(t *testing.T, client *mail.Client, _ []uint32) {
			t.Helper()

			_, err := client.Append("INBOX", raw("a@x", "b@y", "z"), nil, time.Time{})
			require.Error(t, err)
		}},
		{"LIST", func(t *testing.T, client *mail.Client, _ []uint32) {
			t.Helper()
			require.Error(t, client.Ensure("New"))
		}},
		{"CREATE", func(t *testing.T, client *mail.Client, _ []uint32) {
			t.Helper()
			require.Error(t, client.Ensure("New"))
		}},
		{"STATUS", func(t *testing.T, client *mail.Client, _ []uint32) {
			t.Helper()

			_, err := client.State("INBOX")
			require.Error(t, err)
		}},
	}

	for _, tt := range tests {
		t.Run(tt.op, func(t *testing.T) {
			t.Parallel()

			var failing atomic.Bool

			srv := imaptest.StartWith(t, imaptest.Options{
				Caps: imaptest.Rev2(),
				Fail: func(op, _ string) bool { return failing.Load() && op == tt.op },
			})
			client := connect(t, srv)
			uids := seed(t, client, "INBOX", raw("a@x", "b@y", "m"))

			failing.Store(true)

			tt.check(t, client, uids)
		})
	}
}

func TestFindMessageID(t *testing.T) {
	t.Parallel()

	// Each case seeds UIDs 1 and 2 into a fresh mailbox.
	tests := []struct {
		name      string
		messageID string
		want      []uint32
	}{
		{"match", "<second@test>", []uint32{2}},
		{"no match", "<missing@test>", nil},
		{"empty id", "", nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			client := connect(t, imaptest.Start(t, imaptest.Rev2()))
			seed(t, client, "INBOX", raw("a@x", "b@y", "first"), raw("a@x", "b@y", "second"))

			got, err := client.FindMessageID("INBOX", tt.messageID)
			require.NoError(t, err)

			if tt.want == nil {
				require.Empty(t, got)

				return
			}

			require.Equal(t, tt.want, got)
		})
	}
}

const (
	junkKeyword      = "$Junk"
	appleJunkKeyword = "Junk"
)

func TestModStateWithoutCondstore(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	_, ok, err := client.ModState("INBOX")
	require.NoError(t, err)
	require.False(t, ok)
}

func TestSearchKeywords(t *testing.T) {
	t.Parallel()

	old := time.Now().AddDate(0, 0, -60)

	tests := []struct {
		name     string
		keywords []string
		since    time.Time
		want     []uint32
	}{
		{"one keyword", []string{junkKeyword}, time.Time{}, []uint32{1}},
		{"either keyword", []string{junkKeyword, appleJunkKeyword}, time.Time{}, []uint32{1, 2}},
		{
			"recent only",
			[]string{junkKeyword, appleJunkKeyword},
			time.Now().AddDate(0, 0, -30),
			[]uint32{1},
		},
		{"no keywords", nil, time.Time{}, nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			client := connect(t, imaptest.Start(t, imaptest.Rev2()))

			// UID 1: $Junk, recent. UID 2: Junk, old. UID 3: unflagged.
			_, err := client.Append(
				"INBOX",
				raw("a@x", "b@y", "one"),
				[]string{junkKeyword},
				time.Now(),
			)
			require.NoError(t, err)
			_, err = client.Append(
				"INBOX",
				raw("a@x", "b@y", "two"),
				[]string{appleJunkKeyword},
				old,
			)
			require.NoError(t, err)
			_, err = client.Append("INBOX", raw("a@x", "b@y", "three"), nil, time.Now())
			require.NoError(t, err)

			got, err := client.SearchKeywords("INBOX", tt.keywords, tt.since, 0)
			require.NoError(t, err)

			if tt.want == nil {
				require.Empty(t, got)

				return
			}

			require.Equal(t, tt.want, got)
		})
	}
}

func TestFetchEnvelopes(t *testing.T) {
	t.Parallel()

	client := connect(t, imaptest.Start(t, imaptest.Rev2()))

	uid, err := client.Append(
		"INBOX",
		raw("alice@example.com", "b@y", "hello"),
		[]string{junkKeyword},
		time.Time{},
	)
	require.NoError(t, err)

	messages, err := client.FetchEnvelopes("INBOX", []uint32{uid})
	require.NoError(t, err)
	require.Len(t, messages, 1)
	require.Equal(t, "<hello@test>", messages[0].MessageID)
	require.Equal(t, "alice@example.com", messages[0].From)
	require.Contains(t, messages[0].Flags, "$junk")
	require.Empty(t, messages[0].Raw, "no body is downloaded")
}

func TestCloseWhenDoneUnblocksAHungServer(t *testing.T) {
	t.Parallel()

	var (
		entered sync.Once
		inside  = make(chan struct{})
		release = make(chan struct{})
	)

	srv := imaptest.StartWith(t, imaptest.Options{
		Caps: imaptest.Rev2(),
		Fail: func(op, _ string) bool {
			if op == "STATUS" {
				entered.Do(func() { close(inside) })
				<-release
			}

			return false
		},
	})
	t.Cleanup(func() { close(release) })

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr: srv.Addr, Username: imaptest.Username, Password: imaptest.Password, Plaintext: true,
	})
	require.NoError(t, err)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	stop := client.CloseWhenDone(ctx)
	defer stop()

	failed := make(chan error, 1)

	go func() {
		_, err := client.State("INBOX")
		failed <- err
	}()

	<-inside
	cancel()

	select {
	case err := <-failed:
		require.Error(t, err)
	case <-time.After(5 * time.Second):
		t.Fatal("State still blocked after the context ended")
	}
}

func TestDebugLogsExchanges(t *testing.T) {
	t.Parallel()

	var logs bytes.Buffer

	srv := imaptest.Start(t, imaptest.Rev2())

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr:      srv.Addr,
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		Plaintext: true,
		Logger: slog.New(
			slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}),
		),
	})
	require.NoError(t, err)

	t.Cleanup(func() { require.NoError(t, client.Close()) })

	uids := seed(t, client, "INBOX", raw("a@x", "b@y", "m1"))

	_, err = client.Fetch("INBOX", uids)
	require.NoError(t, err)
	require.NoError(t, client.AddKeyword("INBOX", uids, "$Test"))
	require.NoError(t, client.Move("INBOX", uids, "Junk"))

	for _, want := range []string{
		"msg=connected",
		"msg=append mailbox=INBOX",
		"msg=select mailbox=INBOX",
		"msg=fetch mailbox=INBOX requested=1 returned=1",
		"msg=store mailbox=INBOX keyword=$Test",
		"msg=move mailbox=INBOX dest=Junk messages=1",
	} {
		require.Contains(t, logs.String(), want)
	}

	require.NotContains(t, logs.String(), imaptest.Password)
	require.NotContains(t, logs.String(), imaptest.Username)
}

func TestWaitForNewMail(t *testing.T) {
	t.Parallel()

	const maxWait = 2 * time.Second

	tests := []struct {
		name string
		// prepare runs after the caller's state is noted and before the
		// wait, and may return a function to run shortly after it starts.
		prepare func(t *testing.T, client *mail.Client, seen *mail.State) func()
		quick   bool
	}{
		{
			name: "message arrives",
			prepare: func(t *testing.T, client *mail.Client, _ *mail.State) func() {
				t.Helper()

				return func() { seed(t, client, "INBOX", raw("a@x", "b@y", "pushed")) }
			},
			quick: true,
		},
		{
			name:    "nothing arrives",
			prepare: func(*testing.T, *mail.Client, *mail.State) func() { return nil },
			quick:   false,
		},
		{
			name: "a message is marked",
			prepare: func(t *testing.T, client *mail.Client, seen *mail.State) func() {
				t.Helper()
				uids := seed(t, client, "INBOX", raw("a@x", "b@y", "marked later"))

				state, err := client.State("INBOX")
				require.NoError(t, err)

				*seen = state

				return func() { require.NoError(t, client.AddKeyword("INBOX", uids, "Junk")) }
			},
			quick: true,
		},
		{
			name: "message arrived before the wait",
			prepare: func(t *testing.T, client *mail.Client, _ *mail.State) func() {
				t.Helper()
				seed(t, client, "INBOX", raw("a@x", "b@y", "early"))

				return nil
			},
			quick: true,
		},
		{
			name: "mailbox was replaced",
			prepare: func(_ *testing.T, _ *mail.Client, seen *mail.State) func() {
				seen.UIDValidity++

				return nil
			},
			quick: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())
			client := connect(t, srv)

			seen, err := client.State("INBOX")
			require.NoError(t, err)

			if later := tt.prepare(t, client, &seen); later != nil {
				time.AfterFunc(200*time.Millisecond, later)
			}

			start := time.Now()
			err = mail.WaitForNewMail(t.Context(), dialConfig(srv), "INBOX", seen, maxWait)
			elapsed := time.Since(start)

			require.NoError(t, err)

			if tt.quick {
				require.Less(t, elapsed, maxWait)
			} else {
				require.GreaterOrEqual(t, elapsed, maxWait)
			}
		})
	}
}

func TestWaitForNewMailWithoutIdle(t *testing.T) {
	t.Parallel()

	listener, err := (&net.ListenConfig{}).Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)
	t.Cleanup(func() { _ = listener.Close() })

	go serveWithoutIdle(listener)

	cfg := mail.DialConfig{
		Addr:      listener.Addr().String(),
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		Plaintext: true,
	}

	err = mail.WaitForNewMail(t.Context(), cfg, "INBOX", mail.State{}, time.Second)
	require.ErrorIs(t, err, mail.ErrNoIdle)
}

// serveWithoutIdle answers every command with OK and advertises only
// IMAP4rev1, which is all a client needs to log in and see IDLE is missing.
func serveWithoutIdle(listener net.Listener) {
	conn, err := listener.Accept()
	if err != nil {
		return
	}

	defer func() { _ = conn.Close() }()

	_, _ = fmt.Fprint(conn, "* OK [CAPABILITY IMAP4rev1] ready\r\n")

	scanner := bufio.NewScanner(conn)
	for scanner.Scan() {
		tag, _, _ := strings.Cut(scanner.Text(), " ")
		_, _ = fmt.Fprintf(conn, "%s OK [CAPABILITY IMAP4rev1] done\r\n", tag)
	}
}

func TestWaitForNewMailStopsWithContext(t *testing.T) {
	t.Parallel()

	srv := imaptest.Start(t, imaptest.Rev2())
	ctx, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)

	defer cancel()

	start := time.Now()
	err := mail.WaitForNewMail(ctx, dialConfig(srv), "INBOX", currentState(t, srv), time.Minute)

	require.NoError(t, err)
	require.Less(t, time.Since(start), 5*time.Second)
}

func currentState(t *testing.T, srv *imaptest.Server) mail.State {
	t.Helper()

	state, err := connect(t, srv).State("INBOX")
	require.NoError(t, err)

	return state
}

func dialConfig(srv *imaptest.Server) mail.DialConfig {
	return mail.DialConfig{
		Addr:      srv.Addr,
		Username:  imaptest.Username,
		Password:  imaptest.Password,
		Plaintext: srv.TLSConfig == nil,
		TLSConfig: srv.TLSConfig,
	}
}

const (
	idleStarted = "msg=idle "
	idleEnded   = `msg="idle ended"`
)

func TestWaitForNewMailLogsWhatItDoes(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		maxWait time.Duration
		// prepare returns the context to wait under, and may add mail.
		prepare func(t *testing.T, client *mail.Client, seen *mail.State) context.Context
		want    []string
		notWant []string
	}{
		{
			name:    "new mail arrives",
			maxWait: 5 * time.Second,
			prepare: func(t *testing.T, client *mail.Client, _ *mail.State) context.Context {
				t.Helper()
				time.AfterFunc(200*time.Millisecond, func() {
					_, err := client.Append("INBOX", raw("a@x", "b@y", "pushed"), nil, time.Time{})
					assert.NoError(t, err)
				})

				return t.Context()
			},
			want: []string{idleStarted, "max_wait=5s", idleEnded, `reason="new mail"`},
		},
		{
			name:    "nothing arrives",
			maxWait: 300 * time.Millisecond,
			prepare: func(t *testing.T, _ *mail.Client, _ *mail.State) context.Context {
				t.Helper()

				return t.Context()
			},
			want: []string{idleStarted, idleEnded, "reason=timeout"},
		},
		{
			name:    "mail was already waiting",
			maxWait: 5 * time.Second,
			prepare: func(t *testing.T, client *mail.Client, _ *mail.State) context.Context {
				t.Helper()
				seed(t, client, "INBOX", raw("a@x", "b@y", "early"))

				return t.Context()
			},
			want:    []string{idleEnded, `reason="mail already waiting"`},
			notWant: []string{idleStarted},
		},
		{
			name:    "stopped from outside",
			maxWait: time.Minute,
			prepare: func(t *testing.T, _ *mail.Client, _ *mail.State) context.Context {
				t.Helper()

				ctx, cancel := context.WithTimeout(t.Context(), 300*time.Millisecond)
				t.Cleanup(cancel)

				return ctx
			},
			want: []string{idleStarted, idleEnded, "reason=stopped"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())
			client := connect(t, srv)

			seen, err := client.State("INBOX")
			require.NoError(t, err)

			ctx := tt.prepare(t, client, &seen)

			var logs bytes.Buffer

			cfg := dialConfig(srv)
			cfg.Logger = slog.New(
				slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}),
			)

			require.NoError(t, mail.WaitForNewMail(ctx, cfg, "INBOX", seen, tt.maxWait))

			for _, want := range tt.want {
				require.Contains(t, logs.String(), want)
			}

			for _, notWant := range tt.notWant {
				require.NotContains(t, logs.String(), notWant)
			}

			require.Contains(
				t,
				logs.String(),
				"msg=disconnected",
				"closing the connection is logged",
			)
		})
	}
}

func TestWaitForNewMailInAnyFolder(t *testing.T) {
	t.Parallel()

	const (
		maxWait = 2 * time.Second
		second  = "Work"
	)

	tests := []struct {
		name string
		// prepare runs before the wait and may return a function to run shortly after it starts.
		prepare func(t *testing.T, client *mail.Client) func()
		quick   bool
	}{
		{
			name: "message arrives in the second folder",
			prepare: func(t *testing.T, client *mail.Client) func() {
				t.Helper()

				return func() { seed(t, client, second, raw("a@x", "b@y", "pushed")) }
			},
			quick: true,
		},
		{
			name: "message arrived in the second folder before the wait",
			prepare: func(t *testing.T, client *mail.Client) func() {
				t.Helper()
				seed(t, client, second, raw("a@x", "b@y", "early"))

				return nil
			},
			quick: true,
		},
		{
			name:    "no mail arrives",
			prepare: func(*testing.T, *mail.Client) func() { return nil },
			quick:   false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())
			client := connect(t, srv)
			require.NoError(t, client.Ensure(second))

			watches := make([]mail.Watch, 0, 2)

			for _, folder := range []string{"INBOX", second} {
				state, err := client.State(folder)
				require.NoError(t, err)

				watches = append(watches, mail.Watch{Mailbox: folder, Seen: state})
			}

			if later := tt.prepare(t, client); later != nil {
				time.AfterFunc(200*time.Millisecond, later)
			}

			start := time.Now()
			err := mail.WaitForNewMailIn(t.Context(), dialConfig(srv), watches, maxWait)
			elapsed := time.Since(start)

			require.NoError(t, err)

			if tt.quick {
				require.Less(t, elapsed, maxWait)
			} else {
				require.GreaterOrEqual(t, elapsed, maxWait)
			}
		})
	}
}

func TestWaitForNewMailInSurvivesAFailingFolder(t *testing.T) {
	t.Parallel()

	const maxWait = 2 * time.Second

	tests := []struct {
		name    string
		folders []string
		arrives bool
		wantErr bool
	}{
		{"another folder still hears of mail", []string{"Missing", "INBOX"}, true, false},
		{"every folder fails", []string{"Missing", "AlsoMissing"}, false, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			srv := imaptest.Start(t, imaptest.Rev2())
			client := connect(t, srv)

			state, err := client.State("INBOX")
			require.NoError(t, err)

			watches := make([]mail.Watch, 0, len(tt.folders))
			for _, folder := range tt.folders {
				watches = append(watches, mail.Watch{Mailbox: folder, Seen: state})
			}

			if tt.arrives {
				time.AfterFunc(200*time.Millisecond, func() {
					seed(t, client, "INBOX", raw("a@x", "b@y", "pushed"))
				})
			}

			start := time.Now()
			err = mail.WaitForNewMailIn(t.Context(), dialConfig(srv), watches, maxWait)

			if tt.wantErr {
				require.Error(t, err)

				return
			}

			require.NoError(t, err)
			require.Less(t, time.Since(start), maxWait)
		})
	}
}

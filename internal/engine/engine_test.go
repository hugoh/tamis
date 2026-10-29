package engine_test

import (
	"bytes"
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/engine"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const (
	seenFlag = `\Seen`
	opFetch  = "FETCH"
	opMove   = "MOVE"
	opStore  = "STORE"
	opSelect = "SELECT"
	inTrain  = "train from"
)

const (
	inbox     = "INBOX"
	junk      = "Junk"
	sent      = "Sent Messages"
	trainSpam = "TrainSpam"
	trainGood = "TrainGood"
)

type env struct {
	client *mail.Client
	db     *store.Store
	cfg    *config.Config
	engine *engine.Engine
	serial int
}

func setup(t *testing.T, mode config.Mode) *env {
	t.Helper()

	return setupWith(t, mode, nil)
}

// setupWith makes the server fail the commands fail selects.
func setupWith(t *testing.T, mode config.Mode, fail func(op, mailbox string) bool) *env {
	t.Helper()

	srv := imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), Fail: fail})

	client, err := mail.Dial(t.Context(), mail.DialConfig{
		Addr: srv.Addr, Username: imaptest.Username, Password: imaptest.Password, Plaintext: true,
	})
	require.NoError(t, err)
	t.Cleanup(func() { _ = client.Close() })

	db, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "tamis.db"))
	require.NoError(t, err)
	t.Cleanup(func() { _ = db.Close() })

	cfg := &config.Config{
		Folders: config.Folders{
			Watch: []string{
				inbox,
			},
			Junk:      junk,
			Sent:      sent,
			TrainSpam: trainSpam,
			TrainGood: trainGood,
		},
		Policy: config.Policy{
			Mode: mode, SpamThreshold: 0.9, GoodThreshold: 0.2, WhitelistRequiresAuth: true,
			LearnFromJunk: true, LearnFromKeywords: true, Rescue: true,
		},
	}

	e := &env{client: client, db: db, cfg: cfg}
	e.engine = engine.New(client, db, cfg, slog.New(slog.DiscardHandler))

	return e
}

// spammy and hammy messages share vocabulary the model is trained on.
func (e *env) spammy(from string) []byte {
	return e.message(
		from,
		"WINNER claim prize",
		"Congratulations winner, claim your lottery prize now.",
	)
}

func (e *env) hammy(from string) []byte {
	return e.message(from, "Project meeting", "Agenda for the project meeting with the team.")
}

func (e *env) message(from, subject, body string, headers ...string) []byte {
	e.serial++

	var extra strings.Builder
	for _, header := range headers {
		extra.WriteString(header + "\r\n")
	}

	return fmt.Appendf(nil,
		"%sFrom: %s\r\nTo: me@example.com\r\nSubject: %s\r\nMessage-ID: <m%d@test>\r\n"+
			"Content-Type: text/plain\r\n\r\n%s\r\n",
		extra.String(), from, subject, e.serial, body)
}

func (e *env) trainModel(t *testing.T) {
	t.Helper()

	for i := range 10 {
		_, err := e.db.Train(t.Context(), fmt.Sprintf("spam%d", i),
			classify.Tokenize(e.spammy("x@spam.example")), classify.Spam)
		require.NoError(t, err)
		_, err = e.db.Train(t.Context(), fmt.Sprintf("good%d", i),
			classify.Tokenize(e.hammy("y@good.example")), classify.Good)
		require.NoError(t, err)
	}
}

func (e *env) append(t *testing.T, mailbox string, raw []byte) {
	t.Helper()

	require.NoError(t, e.client.Ensure(mailbox))

	_, err := e.client.Append(mailbox, raw, nil, time.Time{})
	require.NoError(t, err)
}

func (e *env) run(t *testing.T) engine.Summary {
	t.Helper()

	summary, err := e.engine.RunOnce(t.Context())
	require.NoError(t, err)

	return summary
}

func (e *env) messages(t *testing.T, mailbox string) []mail.Message {
	t.Helper()

	uids, err := e.client.UIDsAfter(mailbox, 0)
	require.NoError(t, err)

	messages, err := e.client.Fetch(mailbox, uids)
	require.NoError(t, err)

	return messages
}

func (e *env) decisions(t *testing.T) []store.Decision {
	t.Helper()

	decisions, err := e.db.Decisions(t.Context(), 100)
	require.NoError(t, err)

	return decisions
}

func TestFirstRunStartsFromCurrentMail(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.append(t, inbox, e.spammy("old@spam.example"))

	summary := e.run(t)

	require.Zero(t, summary.Scanned, "mail already in the inbox is left to bootstrap")
	require.Len(t, e.messages(t, inbox), 1)
}

func TestShadowModeChangesNothing(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("new@spam.example"))

	summary := e.run(t)

	require.Equal(t, engine.Summary{Scanned: 1, Spam: 1}, summary)
	require.Len(t, e.messages(t, inbox), 1, "shadow mode must not move mail")
	require.Empty(t, e.messages(t, junk))

	decisions := e.decisions(t)
	require.Len(t, decisions, 1)
	require.Equal(t, "spam", decisions[0].Verdict)
	require.Equal(t, "move:"+junk, decisions[0].Action)
	require.False(t, decisions[0].Applied)
	require.Equal(t, "new@spam.example", decisions[0].From)
}

func TestLiveModeActsOnEachVerdict(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)

	e.append(t, inbox, e.spammy("s@spam.example"))
	e.append(t, inbox, e.hammy("g@good.example"))
	e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."))

	summary := e.run(t)

	require.Equal(t, engine.Summary{Scanned: 3, Spam: 1, Good: 1, Uncertain: 1}, summary)

	filed := e.messages(t, junk)
	require.Len(t, filed, 1)
	require.Equal(t, "s@spam.example", filed[0].From)
	require.Contains(t, filed[0].Flags, "$tamisfiled")

	kept := e.messages(t, inbox)
	require.Len(t, kept, 2)

	for _, message := range kept {
		if message.From == "u@unknown.example" {
			require.Contains(t, message.Flags, "$tamisuncertain")
		} else {
			require.NotContains(t, message.Flags, "$tamisuncertain")
		}
	}

	for _, decision := range e.decisions(t) {
		require.Equal(t, decision.Action != "none", decision.Applied, decision.Verdict)
	}
}

func TestSecondRunDoesNotRescan(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.hammy("g@good.example"))
	e.run(t)

	require.Equal(t, engine.Summary{}, e.run(t))
}

func TestWhitelist(t *testing.T) {
	t.Parallel()

	const (
		friend = "friend@example.com"
		pass   = "Authentication-Results: mx.test; dmarc=pass header.from=example.com"
	)

	tests := []struct {
		name        string
		headers     []string
		requireAuth bool
		want        string
		reason      string
	}{
		{"verified friend", []string{pass}, true, engine.VerdictGood, engine.ReasonWhitelisted},
		{"unverified friend", nil, true, "spam", "bayes"},
		{
			"unverified friend, auth not required",
			nil,
			false,
			engine.VerdictGood,
			engine.ReasonWhitelisted,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.cfg.Policy.WhitelistRequiresAuth = tt.requireAuth
			e.trainModel(t)
			require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
			e.run(t)

			e.append(t, inbox, e.message(friend, "WINNER claim prize",
				"Congratulations winner, claim your lottery prize now.", tt.headers...))
			e.run(t)

			decision := e.decisions(t)[0]
			require.Equal(t, tt.want, decision.Verdict)
			require.Equal(t, tt.reason, decision.Reason)
		})
	}
}

func TestTrainFolders(t *testing.T) {
	t.Parallel()

	// Training folders are an explicit request, so they work in shadow mode too.
	e := setup(t, config.ModeShadow)
	e.run(t)

	e.append(t, trainSpam, e.spammy("s@spam.example"))
	e.append(t, trainGood, e.hammy("Friend@Good.example"))

	summary := e.run(t)
	require.Equal(t, 2, summary.Trained)

	stats, err := e.db.Stats(t.Context())
	require.NoError(t, err)
	require.Equal(t, 1, stats.Spam)
	require.Equal(t, 1, stats.Good)

	require.Empty(t, e.messages(t, trainSpam))
	require.Empty(t, e.messages(t, trainGood))
	require.Len(t, e.messages(t, junk), 1)
	require.Len(t, e.messages(t, inbox), 1)

	whitelisted, err := e.db.HasAddress(t.Context(), "friend@good.example")
	require.NoError(t, err)
	require.True(t, whitelisted, "training as good whitelists the sender")

	require.Zero(t, e.run(t).Scanned, "mail returned to the inbox by training is not judged again")
}

func TestTrainFoldersRecordTheRescore(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		folder string
		mail   func(e *env) []byte
		spammy bool
	}{
		{"spam", trainSpam, func(e *env) []byte { return e.spammy("s@spam.example") }, true},
		{"good", trainGood, func(e *env) []byte { return e.hammy("f@good.example") }, false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.trainModel(t)
			e.run(t)
			e.append(t, tt.folder, tt.mail(e))
			e.run(t)

			score := e.decisions(t)[0].Score
			require.NotZero(t, score)
			require.Equal(t, tt.spammy, score > 0.5)
		})
	}
}

func TestReusedMessageIDIsStillJudged(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)

	e.append(t, trainGood, e.hammy("friend@good.example"))
	require.Equal(t, engine.Summary{Trained: 1}, e.run(t))

	trainedID := e.messages(t, inbox)[0].MessageID

	spam := e.spammy("s@spam.example")
	spam = bytes.Replace(spam, fmt.Appendf(nil, "<m%d@test>", e.serial), []byte(trainedID), 1)
	e.append(t, inbox, spam)

	require.Equal(t, engine.Summary{Scanned: 1, Spam: 1}, e.run(t))
}

func TestTrainFoldersAreCreated(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.run(t)

	for _, folder := range []string{trainSpam, trainGood} {
		_, err := e.client.State(folder)
		require.NoError(t, err)
	}
}

func TestCorrection(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)

	e.append(t, inbox, e.spammy("actually@friend.example"))
	require.Equal(t, 1, e.run(t).Spam)

	filed := e.messages(t, junk)
	require.Len(t, filed, 1)

	// The user disagrees and moves it back.
	require.NoError(t, e.client.Move(junk, []uint32{filed[0].UID}, inbox))

	summary := e.run(t)
	require.Equal(t, engine.Summary{Scanned: 1, Corrected: 1}, summary)
	require.Len(t, e.messages(t, inbox), 1, "a corrected message stays where the user put it")

	decision := e.decisions(t)[0]
	require.Equal(t, "good", decision.Verdict)
	require.Equal(t, "correction", decision.Reason)
}

func TestUIDValidityChangeRestartsFolder(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.cfg.Folders.Watch = []string{"Lists"}
	e.append(t, "Lists", e.hammy("a@b.example"))
	e.run(t)

	cursor, ok, err := e.db.Cursor(t.Context(), "Lists")
	require.NoError(t, err)
	require.True(t, ok)

	require.NoError(t, e.db.SetCursor(t.Context(), "Lists",
		store.Cursor{UIDValidity: cursor.UIDValidity + 1000, LastUID: 0}))

	require.Zero(t, e.run(t).Scanned, "a new UIDVALIDITY restarts from current mail")

	reset, _, err := e.db.Cursor(t.Context(), "Lists")
	require.NoError(t, err)
	require.Equal(t, cursor, reset)
}

func TestBootstrap(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	old := time.Now().AddDate(0, 0, -30)

	for _, to := range []string{"friend@example.com", "colleague@example.org"} {
		e.append(t, sent, fmt.Appendf(nil,
			"From: me@example.com\r\nTo: %s\r\nSubject: hi\r\n\r\nhello\r\n", to))
	}

	e.append(t, junk, e.spammy("a@spam.example"))
	e.append(t, junk, e.spammy("b@spam.example"))

	_, err := e.client.Append(inbox, e.hammy("old@good.example"), []string{seenFlag}, old)
	require.NoError(t, err)
	_, err = e.client.Append(inbox, e.hammy("unread@good.example"), nil, old)
	require.NoError(t, err)
	_, err = e.client.Append(inbox, e.hammy("recent@good.example"), []string{seenFlag}, time.Now())
	require.NoError(t, err)

	require.NoError(t, e.client.Ensure("Archive"))
	_, err = e.client.Append("Archive", e.hammy("archived@good.example"), []string{seenFlag}, old)
	require.NoError(t, err)

	summary, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		SentLimit:  100,
		Spam:       []engine.Source{{Folder: junk, Limit: 100}},
		Good:       []engine.Source{{Folder: inbox, Limit: 100}, {Folder: "Archive", Limit: 100}},
		GoodMinAge: 7 * 24 * time.Hour,
	})
	require.NoError(t, err)
	require.Equal(t, engine.BootstrapSummary{Addresses: 2, Spam: 2, Good: 2}, summary)

	for _, address := range []string{"friend@example.com", "colleague@example.org"} {
		ok, err := e.db.HasAddress(t.Context(), address)
		require.NoError(t, err)
		require.True(t, ok, address)
	}

	require.Zero(t, e.run(t).Scanned, "bootstrap starts watching from current mail")
}

func TestBootstrapLogsProgress(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)

	e.append(
		t,
		sent,
		[]byte("From: me@example.com\r\nTo: friend@example.com\r\nSubject: hi\r\n\r\nhello\r\n"),
	)
	e.append(t, junk, e.spammy("a@spam.example"))
	e.append(t, junk, e.spammy("b@spam.example"))

	var logs bytes.Buffer

	logged := engine.New(
		e.client,
		e.db,
		e.cfg,
		slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug})),
	)

	_, err := logged.Bootstrap(t.Context(), engine.BootstrapOptions{
		SentLimit: 100,
		Spam:      []engine.Source{{Folder: junk, Limit: 100}},
	})
	require.NoError(t, err)

	for _, want := range []string{
		"msg=\"whitelisting recipients\" folder=\"Sent Messages\" messages=1",
		"msg=training folder=Junk label=spam messages=2",
		"msg=\"training progress\" folder=Junk done=2 total=2",
	} {
		require.Contains(t, logs.String(), want)
	}
}

func TestBootstrapKeepCursors(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.append(t, junk, e.spammy("a@spam.example"))

	_, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Spam:        []engine.Source{{Folder: junk, Limit: 100}},
		KeepCursors: true,
	})
	require.NoError(t, err)

	_, ok, err := e.db.Cursor(t.Context(), inbox)
	require.NoError(t, err)
	require.False(t, ok, "a bootstrap of other folders must not skip unjudged inbox mail")
}

func TestBootstrapLimits(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)

	for range 3 {
		e.append(t, junk, e.spammy("a@spam.example"))
	}

	summary, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Spam: []engine.Source{{Folder: junk, Limit: 2}},
	})
	require.NoError(t, err)
	require.Equal(t, 2, summary.Spam)
}

func TestBootstrapMissingFolder(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)

	_, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
		Good: []engine.Source{{Folder: "Nope", Limit: 10}},
	})
	require.ErrorContains(t, err, "Nope")
}

func TestRunOnceFailures(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		fail    func(op, mailbox string) bool
		prepare func(t *testing.T, e *env)
		want    string
	}{
		{
			name: "train folder can't be listed",
			fail: func(op, _ string) bool { return op == "LIST" },
			want: inTrain,
		},
		{
			name: "train folder can't be fetched",
			fail: func(op, _ string) bool { return op == opFetch },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, trainSpam, e.spammy("s@spam.example"))
			},
			want: inTrain,
		},
		{
			name: "trained mail can't be moved",
			fail: func(op, _ string) bool { return op == opMove },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, trainGood, e.hammy("g@good.example"))
			},
			want: inTrain,
		},
		{
			name: "watched folder status fails",
			fail: func(op, mailbox string) bool { return op == "STATUS" && mailbox == inbox },
			want: "scan INBOX",
		},
		{
			name: "new mail can't be fetched",
			fail: func(op, _ string) bool { return op == opFetch },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
			},
			want: "scan INBOX",
		},
		{
			name: "spam can't be flagged",
			fail: func(op, _ string) bool { return op == opStore },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
			},
			want: "file spam",
		},
		{
			name: "spam can't be moved",
			fail: func(op, mailbox string) bool { return op == opMove && mailbox == junk },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
			},
			want: "file spam",
		},
		{
			name: "uncertain mail can't be flagged",
			fail: func(op, _ string) bool { return op == opStore },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra."))
			},
			want: "flag uncertain",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			var failing atomic.Bool

			e := setupWith(t, config.ModeLive, func(op, mailbox string) bool {
				return failing.Load() && tt.fail(op, mailbox)
			})
			e.trainModel(t)
			e.run(t)

			if tt.prepare != nil {
				tt.prepare(t, e)
			}

			failing.Store(true)

			_, err := e.engine.RunOnce(t.Context())
			require.ErrorContains(t, err, tt.want)
		})
	}
}

func TestBootstrapFailures(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		fail func(op, mailbox string) bool
		want string
	}{
		{
			"sent unreadable",
			func(op, mailbox string) bool { return op == opSelect && mailbox == sent },
			sent,
		},
		{
			"junk unreadable",
			func(op, mailbox string) bool { return op == opSelect && mailbox == junk },
			junk,
		},
		{
			"inbox unreadable",
			func(op, mailbox string) bool { return op == opSelect && mailbox == inbox },
			inbox,
		},
		{"junk fetch fails", func(op, _ string) bool { return op == opFetch }, junk},
		{"inbox status fails", func(op, _ string) bool { return op == "STATUS" }, inbox},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setupWith(t, config.ModeShadow, tt.fail)
			require.NoError(t, e.client.Ensure(sent))
			e.append(t, junk, e.spammy("s@spam.example"))

			_, err := e.engine.Bootstrap(t.Context(), engine.BootstrapOptions{
				SentLimit: 10,
				Spam:      []engine.Source{{Folder: junk, Limit: 10}},
				Good:      []engine.Source{{Folder: inbox, Limit: 10}},
			})
			require.ErrorContains(t, err, tt.want)
		})
	}
}

func TestFailedMoveIsNotACorrection(t *testing.T) {
	t.Parallel()

	var failing atomic.Bool

	e := setupWith(t, config.ModeLive, func(op, mailbox string) bool {
		return failing.Load() && op == opMove && mailbox == junk
	})
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("s@spam.example"))

	failing.Store(true)

	_, err := e.engine.RunOnce(t.Context())
	require.ErrorContains(t, err, "file spam")

	failing.Store(false)

	summary := e.run(t)
	require.Equal(
		t,
		engine.Summary{Scanned: 1, Spam: 1},
		summary,
		"retried as spam, not learnt as good",
	)

	whitelisted, err := e.db.HasAddress(t.Context(), "s@spam.example")
	require.NoError(t, err)
	require.False(t, whitelisted)
}

// dropAfterFlag makes Move fail while failing is set and, once it has, every
// STORE too: what a dropped connection does between flagging and moving.
func dropAfterFlag(failing *atomic.Bool) func(op, mailbox string) bool {
	var moveFailed atomic.Bool

	return func(op, _ string) bool {
		if !failing.Load() {
			moveFailed.Store(false)

			return false
		}

		switch op {
		case opMove:
			moveFailed.Store(true)

			return true
		case opStore:
			return moveFailed.Load()
		default:
			return false
		}
	}
}

func TestLeftoverFiledKeywordIsNotACorrection(t *testing.T) {
	t.Parallel()

	var failing atomic.Bool

	e := setupWith(t, config.ModeLive, dropAfterFlag(&failing))
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("s@spam.example"))

	failing.Store(true)

	_, err := e.engine.RunOnce(t.Context())
	require.ErrorContains(t, err, "file spam")

	failing.Store(false)

	require.Equal(t, engine.Summary{Scanned: 1, Spam: 1}, e.run(t))

	whitelisted, err := e.db.HasAddress(t.Context(), "s@spam.example")
	require.NoError(t, err)
	require.False(t, whitelisted)
}

func TestLeftoverRescuedKeywordIsNotACorrection(t *testing.T) {
	t.Parallel()

	var failing atomic.Bool

	e := setupWith(t, config.ModeLive, dropAfterFlag(&failing))
	e.run(t)
	e.fromFriend(t, dmarcOK)

	failing.Store(true)

	_, err := e.engine.RunOnce(t.Context())
	require.ErrorContains(t, err, "rescue")

	failing.Store(false)

	require.Equal(t, engine.Summary{Rescued: 1}, e.run(t))

	whitelisted, err := e.db.HasAddress(t.Context(), friend)
	require.NoError(t, err)
	require.True(t, whitelisted)
}

// latestDecisionID returns the newest journal entry's ID.
func (e *env) latestDecisionID(t *testing.T) int64 {
	t.Helper()

	return e.decisions(t)[0].ID
}

func TestUndoFiling(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("friend@example.org"))
	require.Equal(t, 1, e.run(t).Spam)

	filed := e.latestDecisionID(t)

	summary, err := e.engine.Undo(t.Context(), filed)
	require.NoError(t, err)
	require.Contains(t, summary, "moved back to INBOX")

	require.Empty(t, e.messages(t, junk))

	back := e.messages(t, inbox)
	require.Len(t, back, 1)
	require.NotContains(t, back[0].Flags, "$tamisfiled")

	whitelisted, err := e.db.HasAddress(t.Context(), "friend@example.org")
	require.NoError(t, err)
	require.True(t, whitelisted)

	original, err := e.db.Decision(t.Context(), filed)
	require.NoError(t, err)
	require.True(t, original.Undone)

	undo := e.decisions(t)[0]
	require.Equal(t, engine.ReasonUndo, undo.Reason)
	require.Equal(t, "good", undo.Verdict)

	require.Equal(t, engine.Summary{}, e.run(t), "the restored message is not judged again")
}

func TestUndoUncertain(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra quokka."))
	require.Equal(t, 1, e.run(t).Uncertain)

	summary, err := e.engine.Undo(t.Context(), e.latestDecisionID(t))
	require.NoError(t, err)
	require.Contains(t, summary, engine.UncertainKeyword)

	require.NotContains(t, e.messages(t, inbox)[0].Flags, "$tamisuncertain")
}

func TestUndoRefusals(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		prepare func(t *testing.T, e *env) int64
		want    error
	}{
		{
			name: "unknown decision",
			prepare: func(t *testing.T, _ *env) int64 {
				t.Helper()

				return 999
			},
			want: store.ErrNotFound,
		},
		{
			name: "shadow decision",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()

				e.cfg.Policy.Mode = config.ModeShadow
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				return e.latestDecisionID(t)
			},
			want: engine.ErrNothingToUndo,
		},
		{
			name: "training",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, trainSpam, e.spammy("s@spam.example"))
				e.run(t)

				return e.latestDecisionID(t)
			},
			want: engine.ErrNotUndoable,
		},
		{
			name: "already undone",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				id := e.latestDecisionID(t)
				_, err := e.engine.Undo(t.Context(), id)
				require.NoError(t, err)

				return id
			},
			want: store.ErrAlreadyUndone,
		},
		{
			name: "message gone from Junk",
			prepare: func(t *testing.T, e *env) int64 {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)

				filed := e.messages(t, junk)
				require.NoError(t, e.client.Move(junk, []uint32{filed[0].UID}, trainSpam))

				return e.latestDecisionID(t)
			},
			want: engine.ErrMessageGone,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeLive)
			e.trainModel(t)
			e.run(t)

			_, err := e.engine.Undo(t.Context(), tt.prepare(t, e))
			require.ErrorIs(t, err, tt.want)
		})
	}
}

func TestUndoRetriesAfterFailure(t *testing.T) {
	t.Parallel()

	var failing atomic.Bool

	e := setupWith(t, config.ModeLive, func(op, mailbox string) bool {
		return failing.Load() && op == opMove && mailbox == inbox
	})
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.spammy("s@spam.example"))
	e.run(t)

	filed := e.latestDecisionID(t)

	failing.Store(true)

	_, err := e.engine.Undo(t.Context(), filed)
	require.Error(t, err)

	original, err := e.db.Decision(t.Context(), filed)
	require.NoError(t, err)
	require.False(t, original.Undone, "a failed undo can be retried")

	failing.Store(false)

	_, err = e.engine.Undo(t.Context(), filed)
	require.NoError(t, err)
	require.Len(t, e.messages(t, inbox), 1)
}

func TestUndoServerFailures(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		uncertain bool
		fail      func(op, mailbox string) bool
	}{
		{
			"search fails",
			false,
			func(op, _ string) bool { return op == "SEARCH" },
		},
		{"fetch fails", false, func(op, _ string) bool { return op == opFetch }},
		{"keyword can't be cleared", false, func(op, _ string) bool { return op == opStore }},
		{
			"uncertain keyword can't be cleared",
			true,
			func(op, _ string) bool { return op == opStore },
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			var failing atomic.Bool

			e := setupWith(t, config.ModeLive, func(op, mailbox string) bool {
				return failing.Load() && tt.fail(op, mailbox)
			})
			e.trainModel(t)
			e.run(t)

			if tt.uncertain {
				e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra."))
			} else {
				e.append(t, inbox, e.spammy("s@spam.example"))
			}

			e.run(t)

			id := e.latestDecisionID(t)

			failing.Store(true)

			_, err := e.engine.Undo(t.Context(), id)
			require.Error(t, err)

			decision, err := e.db.Decision(t.Context(), id)
			require.NoError(t, err)
			require.False(t, decision.Undone)
		})
	}
}

func TestUndoUncertainGone(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.trainModel(t)
	e.run(t)
	e.append(t, inbox, e.message("u@unknown.example", "Quokka", "Xylophone zebra."))
	e.run(t)

	moved := e.messages(t, inbox)
	require.NoError(t, e.client.Move(inbox, []uint32{moved[0].UID}, junk))

	_, err := e.engine.Undo(t.Context(), e.latestDecisionID(t))
	require.ErrorIs(t, err, engine.ErrMessageGone)
}

// moveToJunk plays the user dragging a message from the inbox to Junk.
func (e *env) moveToJunk(t *testing.T, from string) {
	t.Helper()

	for _, message := range e.messages(t, inbox) {
		if message.From == from {
			require.NoError(t, e.client.Move(inbox, []uint32{message.UID}, junk))

			return
		}
	}

	t.Fatalf("no message from %s in %s", from, inbox)
}

func TestMissedSpamLearntFromJunk(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		message func(e *env) []byte
		verdict string
	}{
		{
			"judged good",
			func(e *env) []byte { return e.hammy("sneaky@elsewhere.example") },
			engine.VerdictGood,
		},
		{"judged uncertain", func(e *env) []byte {
			return e.message("sneaky@elsewhere.example", "Quokka", "Xylophone zebra.")
		}, "uncertain"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.trainModel(t)
			e.run(t)
			e.append(t, inbox, tt.message(e))
			e.run(t)
			require.Equal(t, tt.verdict, e.decisions(t)[0].Verdict)

			e.moveToJunk(t, "sneaky@elsewhere.example")

			summary := e.run(t)
			require.Equal(t, engine.Summary{Corrected: 1}, summary)

			decision := e.decisions(t)[0]
			require.Equal(t, "spam", decision.Verdict)
			require.Equal(t, engine.ReasonCorrection, decision.Reason)

			label, trained, err := e.db.Trained(t.Context(), decision.MessageID)
			require.NoError(t, err)
			require.True(t, trained)
			require.Equal(t, classify.Spam, label)

			require.Equal(t, engine.Summary{}, e.run(t), "learnt once")
		})
	}
}

func TestMissedSpamUnwhitelistsSender(t *testing.T) {
	t.Parallel()

	const friend = "friend@example.com"

	e := setup(t, config.ModeShadow)
	e.cfg.Policy.WhitelistRequiresAuth = false
	require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
	e.run(t)
	e.append(t, inbox, e.spammy(friend))
	e.run(t)
	require.Equal(t, engine.ReasonWhitelisted, e.decisions(t)[0].Reason)

	e.moveToJunk(t, friend)
	require.Equal(t, 1, e.run(t).Corrected)

	whitelisted, err := e.db.HasAddress(t.Context(), friend)
	require.NoError(t, err)
	require.False(t, whitelisted)
}

func TestJunkArrivalsThatAreNotCorrections(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name    string
		mode    config.Mode
		learn   bool
		prepare func(t *testing.T, e *env)
	}{
		{
			name:  "filed by the provider",
			mode:  config.ModeShadow,
			learn: true,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, junk, e.spammy("provider@spam.example"))
			},
		},
		{
			name:  "filed by tamis",
			mode:  config.ModeLive,
			learn: true,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.spammy("s@spam.example"))
				e.run(t)
			},
		},
		{
			name:  "trained through TrainSpam",
			mode:  config.ModeShadow,
			learn: true,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, trainSpam, e.spammy("s@spam.example"))
				e.run(t)
			},
		},
		{
			name:  "learning from Junk turned off",
			mode:  config.ModeShadow,
			learn: false,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, inbox, e.hammy("sneaky@elsewhere.example"))
				e.run(t)
				e.moveToJunk(t, "sneaky@elsewhere.example")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, tt.mode)
			e.cfg.Policy.LearnFromJunk = tt.learn
			e.trainModel(t)
			e.run(t)
			tt.prepare(t, e)

			require.Zero(t, e.run(t).Corrected)
		})
	}
}

// marked is who sends the messages the keyword tests mark.
const marked = "someone@elsewhere.example"

// mark plays a mail client's Junk / Not Junk button: it sets keyword on the
// message from marked in folder.
func (e *env) mark(t *testing.T, folder, keyword string) {
	t.Helper()

	e.markFrom(t, folder, marked, keyword)
}

// markFrom sets keyword on the message from sender in folder.
func (e *env) markFrom(t *testing.T, folder, sender, keyword string) {
	t.Helper()

	for _, message := range e.messages(t, folder) {
		if message.From == sender {
			require.NoError(t, e.client.AddKeyword(folder, []uint32{message.UID}, keyword))

			return
		}
	}

	t.Fatalf("no message from %s in %s", sender, folder)
}

func TestKeywordCorrections(t *testing.T) {
	t.Parallel()

	const sender = marked

	tests := []struct {
		name    string
		mode    config.Mode
		message func(e *env) []byte
		folder  string
		keyword string
		want    classify.Label
	}{
		{
			"$Junk on good", config.ModeShadow, func(e *env) []byte { return e.hammy(sender) },
			inbox, "$Junk", classify.Spam,
		},
		{"Junk on uncertain", config.ModeShadow, func(e *env) []byte {
			return e.message(sender, "Quokka", "Xylophone zebra.")
		}, inbox, "Junk", classify.Spam},
		{
			"$NotJunk on a shadow spam verdict",
			config.ModeShadow,
			func(e *env) []byte { return e.spammy(sender) },
			inbox,
			"$NotJunk",
			classify.Good,
		},
		{
			"NotJunk on spam filed to Junk",
			config.ModeLive,
			func(e *env) []byte { return e.spammy(sender) },
			junk,
			"NotJunk",
			classify.Good,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, tt.mode)
			e.trainModel(t)
			e.run(t)
			e.append(t, inbox, tt.message(e))
			e.run(t)

			e.mark(t, tt.folder, tt.keyword)

			require.Equal(t, engine.Summary{Corrected: 1}, e.run(t))

			decision := e.decisions(t)[0]
			require.Equal(t, engine.ReasonCorrection, decision.Reason)

			label, trained, err := e.db.Trained(t.Context(), decision.MessageID)
			require.NoError(t, err)
			require.True(t, trained)
			require.Equal(t, tt.want, label)

			whitelisted, err := e.db.HasAddress(t.Context(), sender)
			require.NoError(t, err)
			require.Equal(t, tt.want == classify.Good, whitelisted)

			require.Equal(t, engine.Summary{}, e.run(t), "learnt once")
		})
	}
}

func TestKeywordsIgnored(t *testing.T) {
	t.Parallel()

	const sender = marked

	tests := []struct {
		name    string
		prepare func(t *testing.T, e *env)
	}{
		{"agrees with tamis", func(t *testing.T, e *env) {
			t.Helper()
			e.append(t, inbox, e.spammy(sender))
			e.run(t)
			e.mark(t, inbox, "$Junk")
		}},
		{"never judged by tamis", func(t *testing.T, e *env) {
			t.Helper()
			e.append(t, junk, e.spammy(sender))
			e.mark(t, junk, "$NotJunk")
		}},
		{"marked both ways", func(t *testing.T, e *env) {
			t.Helper()
			e.append(t, inbox, e.hammy(sender))
			e.run(t)
			e.mark(t, inbox, "$Junk")
			e.mark(t, inbox, "$NotJunk")
		}},
		{"outside the window", func(t *testing.T, e *env) {
			t.Helper()

			_, err := e.client.Append(inbox, e.hammy(sender), nil, time.Now().AddDate(0, 0, -60))
			require.NoError(t, err)
			e.run(t)
			e.mark(t, inbox, "$Junk")
		}},
		{"turned off", func(t *testing.T, e *env) {
			t.Helper()

			e.cfg.Policy.LearnFromKeywords = false
			e.append(t, inbox, e.hammy(sender))
			e.run(t)
			e.mark(t, inbox, "$Junk")
		}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, config.ModeShadow)
			e.trainModel(t)
			e.run(t)
			tt.prepare(t, e)

			require.Zero(t, e.run(t).Corrected)
		})
	}
}

const (
	friend   = "friend@example.com"
	dmarcOK  = "Authentication-Results: mx.test; dmarc=pass header.from=example.com"
	rescueTo = "Rescued"
)

// fromFriend is mail from a whitelisted contact that the provider
// nonetheless filed to Junk.
func (e *env) fromFriend(t *testing.T, headers ...string) {
	t.Helper()

	require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
	e.append(t, junk, e.message(friend, "Lunch?", "Are we still on for lunch?", headers...))
}

func TestRescue(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		mode      config.Mode
		configure func(cfg *config.Config)
		prepare   func(t *testing.T, e *env)
		rescued   bool
		reason    string
		dest      string
	}{
		{
			name:    "verified whitelisted sender",
			mode:    config.ModeLive,
			prepare: func(t *testing.T, e *env) { t.Helper(); e.fromFriend(t, dmarcOK) },
			rescued: true, reason: engine.ReasonWhitelisted, dest: inbox,
		},
		{
			name: "unverified whitelisted sender",
			mode: config.ModeLive,
			configure: func(cfg *config.Config) {
				cfg.Policy.WhitelistRequiresAuth = false
			},
			prepare: func(t *testing.T, e *env) { t.Helper(); e.fromFriend(t) },
		},
		{
			name:      "low score with a threshold",
			mode:      config.ModeLive,
			configure: func(cfg *config.Config) { cfg.Policy.RescueThreshold = 0.05 },
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, junk, e.hammy("colleague@good.example"))
			},
			rescued: true, reason: engine.ReasonBayes, dest: inbox,
		},
		{
			name: "low score without a threshold",
			mode: config.ModeLive,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.append(t, junk, e.hammy("colleague@good.example"))
			},
		},
		{
			name:      "to a rescue folder",
			mode:      config.ModeLive,
			configure: func(cfg *config.Config) { cfg.Folders.Rescued = rescueTo },
			prepare:   func(t *testing.T, e *env) { t.Helper(); e.fromFriend(t, dmarcOK) },
			rescued:   true, reason: engine.ReasonWhitelisted, dest: rescueTo,
		},
		{
			name:      "turned off",
			mode:      config.ModeLive,
			configure: func(cfg *config.Config) { cfg.Policy.Rescue = false },
			prepare:   func(t *testing.T, e *env) { t.Helper(); e.fromFriend(t, dmarcOK) },
		},
		{
			name: "marked Junk by the user",
			mode: config.ModeLive,
			prepare: func(t *testing.T, e *env) {
				t.Helper()
				e.fromFriend(t, dmarcOK)
				e.markFrom(t, junk, friend, "$Junk")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, tt.mode)
			if tt.configure != nil {
				tt.configure(e.cfg)
			}

			e.trainModel(t)
			e.run(t)
			tt.prepare(t, e)

			summary := e.run(t)

			if !tt.rescued {
				require.Zero(t, summary.Rescued)
				require.Len(t, e.messages(t, junk), 1)

				return
			}

			require.Equal(t, 1, summary.Rescued)
			require.Empty(t, e.messages(t, junk))

			moved := e.messages(t, tt.dest)
			require.Len(t, moved, 1)
			require.Contains(t, moved[0].Flags, "$tamisrescued")

			decision := e.decisions(t)[0]
			require.Equal(t, engine.VerdictGood, decision.Verdict)
			require.Equal(t, tt.reason, decision.Reason)
			require.Equal(t, "move:"+tt.dest, decision.Action)
			require.True(t, decision.Applied)

			require.Equal(t, engine.Summary{}, e.run(t), "rescued mail is not judged again")
		})
	}
}

func TestRescueInShadowMode(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.run(t)
	e.fromFriend(t, dmarcOK)

	require.Equal(t, engine.Summary{Rescued: 1}, e.run(t))
	require.Len(t, e.messages(t, junk), 1, "shadow mode moves nothing")

	decision := e.decisions(t)[0]
	require.Equal(t, "move:"+inbox, decision.Action)
	require.False(t, decision.Applied)
}

func TestRescueNeverUndoesTheUser(t *testing.T) {
	t.Parallel()

	// The user moves a verified friend's mail to Junk; with learning off,
	// tamis must still not rescue it back.
	e := setup(t, config.ModeLive)
	e.cfg.Policy.LearnFromJunk = false
	require.NoError(t, e.db.AddAddress(t.Context(), friend, "test"))
	e.run(t)
	e.append(t, inbox, e.message(friend, "Lunch?", "Are we still on?", dmarcOK))
	e.run(t)
	e.moveToJunk(t, friend)

	require.Zero(t, e.run(t).Rescued)
	require.Len(t, e.messages(t, junk), 1)
}

func TestRescueCorrection(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.run(t)
	e.fromFriend(t, dmarcOK)
	require.Equal(t, 1, e.run(t).Rescued)

	e.moveToJunk(t, friend)

	require.Equal(t, engine.Summary{Corrected: 1}, e.run(t))

	whitelisted, err := e.db.HasAddress(t.Context(), friend)
	require.NoError(t, err)
	require.False(t, whitelisted)
	require.Len(t, e.messages(t, junk), 1, "not rescued again")
	require.Equal(t, engine.Summary{}, e.run(t))
}

func TestRescueFailures(t *testing.T) {
	t.Parallel()

	for _, op := range []string{opStore, opMove} {
		t.Run(op, func(t *testing.T) {
			t.Parallel()

			var failing atomic.Bool

			e := setupWith(t, config.ModeLive, func(got, _ string) bool {
				return failing.Load() && got == op
			})
			e.run(t)
			e.fromFriend(t, dmarcOK)

			failing.Store(true)

			_, err := e.engine.RunOnce(t.Context())
			require.ErrorContains(t, err, "rescue")

			failing.Store(false)

			require.Equal(t, 1, e.run(t).Rescued, "retried on the next pass")
		})
	}
}

func TestUndoRescue(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeLive)
	e.run(t)
	e.fromFriend(t, dmarcOK)
	require.Equal(t, 1, e.run(t).Rescued)

	rescued := e.latestDecisionID(t)

	summary, err := e.engine.Undo(t.Context(), rescued)
	require.NoError(t, err)
	require.Contains(t, summary, "moved back to Junk")

	back := e.messages(t, junk)
	require.Len(t, back, 1)
	require.NotContains(t, back[0].Flags, "$tamisrescued")
	require.Empty(t, e.messages(t, inbox))

	label, trained, err := e.db.Trained(t.Context(), back[0].MessageID)
	require.NoError(t, err)
	require.True(t, trained)
	require.Equal(t, classify.Spam, label)

	whitelisted, err := e.db.HasAddress(t.Context(), friend)
	require.NoError(t, err)
	require.False(t, whitelisted)

	require.Equal(t, engine.Summary{}, e.run(t), "not rescued again")
}

package store_test

import (
	"fmt"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/store"
	"github.com/stretchr/testify/require"
)

const (
	addrAlice    = "alice@example.com"
	addrBob      = "bob@other.example"
	addrCarol    = "carol@example.com"
	addrReal     = "100%_real@example.com"
	sourceManual = "manual"
	actionJunk   = "move:Junk"
)

const (
	subjectFirst  = "first"
	subjectSecond = "second"
	subjectThird  = "third"
)

const (
	subjectLunch   = "Lunch"
	subjectWinner  = "50% off winner"
	subjectReLunch = "Re: Lunch"
	subjectDigest  = "Weekly digest"
	subjectPrize   = "Prize_claim"
)

// journal records five decisions, one minute apart, oldest first.
func journal(t *testing.T) *store.Store {
	t.Helper()

	db := open(t)
	base := time.Date(2026, 10, 30, 12, 0, 0, 0, time.UTC)

	entries := []store.Decision{
		{
			From:    addrAlice,
			Subject: subjectLunch,
			Score:   0.05,
			Verdict: verdictGood,
			Reason:  reasonBayes,
			Action:  actionNone,
		},
		{
			From:    "spam@bad.example",
			Subject: subjectWinner,
			Score:   0.97,
			Verdict: verdictSpam,
			Reason:  reasonBayes,
			Action:  actionJunk,
		},
		{
			From:    "bob@example.com",
			Subject: subjectReLunch,
			Score:   0.05,
			Verdict: verdictGood,
			Reason:  "whitelisted",
			Action:  actionNone,
		},
		{
			From:    "news@list.example",
			Subject: subjectDigest,
			Score:   0.5,
			Verdict: "uncertain",
			Reason:  reasonBayes,
			Action:  "keyword:$Uncertain",
		},
		{
			From:    "spam@bad.example",
			Subject: subjectPrize,
			Score:   0.92,
			Verdict: verdictSpam,
			Reason:  reasonBayes,
			Action:  actionJunk,
		},
	}

	for i, entry := range entries {
		entry.At = base.Add(time.Duration(i) * time.Minute)
		entry.MessageID = fmt.Sprintf("<m%d@example>", i)

		_, err := db.RecordDecision(t.Context(), entry)
		require.NoError(t, err)
	}

	return db
}

func subjects(decisions []store.Decision) []string {
	out := make([]string, len(decisions))
	for i, decision := range decisions {
		out[i] = decision.Subject
	}

	return out
}

func TestQueryDecisions(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		query store.DecisionQuery
		want  []string
		total int
	}{
		{
			"newest first by default",
			store.DecisionQuery{},
			[]string{subjectPrize, subjectDigest, subjectReLunch, subjectWinner, subjectLunch},
			5,
		},
		{
			"oldest first",
			store.DecisionQuery{Sort: store.SortTime, Asc: true},
			[]string{subjectLunch, subjectWinner, subjectReLunch, subjectDigest, subjectPrize},
			5,
		},
		{
			"one verdict",
			store.DecisionQuery{Verdict: verdictSpam},
			[]string{subjectPrize, subjectWinner},
			2,
		},
		{
			"search the subject, ignoring case",
			store.DecisionQuery{Search: "LUNCH"},
			[]string{subjectReLunch, subjectLunch},
			2,
		},
		{
			"search the sender",
			store.DecisionQuery{Search: "bad.example"},
			[]string{subjectPrize, subjectWinner},
			2,
		},
		{
			"search the reason",
			store.DecisionQuery{Search: "whitelisted"},
			[]string{subjectReLunch},
			1,
		},
		{
			"percent is not a wildcard",
			store.DecisionQuery{Search: "50%"},
			[]string{subjectWinner},
			1,
		},
		{
			"underscore is not a wildcard",
			store.DecisionQuery{Search: "prize_c"},
			[]string{subjectPrize},
			1,
		},
		{
			"quotes are just text",
			store.DecisionQuery{Search: "'; DROP TABLE decisions; --"},
			nil, 0,
		},
		{
			"verdict and search together",
			store.DecisionQuery{Verdict: verdictGood, Search: "alice"},
			[]string{subjectLunch},
			1,
		},
		{
			"highest score first, ties newest first",
			store.DecisionQuery{Sort: store.SortScore},
			[]string{subjectWinner, subjectPrize, subjectDigest, subjectReLunch, subjectLunch},
			5,
		},
		{
			"by sender",
			store.DecisionQuery{Sort: store.SortFrom, Asc: true, Limit: 2},
			[]string{subjectLunch, subjectReLunch},
			5,
		},
		{
			"by subject",
			store.DecisionQuery{Sort: store.SortSubject, Asc: true, Limit: 2},
			[]string{subjectWinner, subjectLunch},
			5,
		},
		{
			"an unknown sort falls back to time",
			store.DecisionQuery{Sort: "1; DROP TABLE decisions"},
			[]string{subjectPrize, subjectDigest, subjectReLunch, subjectWinner, subjectLunch},
			5,
		},
		{
			"a page",
			store.DecisionQuery{Limit: 2, Offset: 2},
			[]string{subjectReLunch, subjectWinner},
			5,
		},
		{
			"past the end",
			store.DecisionQuery{Limit: 2, Offset: 10},
			nil, 5,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got, total, err := journal(t).QueryDecisions(t.Context(), tt.query)
			require.NoError(t, err)
			require.Equal(t, tt.total, total, "total counts every match, not just the page")

			if tt.want == nil {
				require.Empty(t, got)

				return
			}

			require.Equal(t, tt.want, subjects(got))
		})
	}
}

func TestQueryDecisionsHasEverySortKey(t *testing.T) {
	t.Parallel()

	db := journal(t)

	for _, sort := range []string{
		store.SortTime, store.SortID, store.SortVerdict, store.SortReason,
		store.SortScore, store.SortAction, store.SortFrom, store.SortSubject,
	} {
		for _, asc := range []bool{true, false} {
			got, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{Sort: sort, Asc: asc})
			require.NoError(t, err, sort)
			require.Len(t, got, 5, sort)
		}
	}
}

func TestQueryDecisionsDefaultLimit(t *testing.T) {
	t.Parallel()

	db := open(t)

	for i := range 120 {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At:      time.Now(),
			Subject: fmt.Sprintf("m%d", i),
			Verdict: verdictGood,
			Reason:  reasonBayes,
			Action:  actionNone,
		})
		require.NoError(t, err)
	}

	got, total, err := db.QueryDecisions(t.Context(), store.DecisionQuery{})
	require.NoError(t, err)
	require.Len(t, got, 100)
	require.Equal(t, 120, total)
}

func TestQueryDecisionsShowsCorrections(t *testing.T) {
	t.Parallel()

	db := journal(t)

	correction, err := db.RecordDecision(t.Context(), store.Decision{
		At:        time.Now(),
		MessageID: "<m1@example>",
		Verdict:   verdictGood,
		Reason:    "correction",
		Action:    "train:good",
	})
	require.NoError(t, err)

	got, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{Search: "winner"})
	require.NoError(t, err)
	require.Len(t, got, 1)
	require.Equal(t, correction, got[0].CorrectedBy)
}

func TestQueryDecisionsFailsOnAClosedStore(t *testing.T) {
	t.Parallel()

	db := open(t)
	require.NoError(t, db.Close())

	_, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{})
	require.Error(t, err)
}

func addressBook(t *testing.T) *store.Store {
	t.Helper()

	db := open(t)

	for _, entry := range []struct{ address, source string }{
		{addrCarol, "sent"},
		{addrAlice, sourceManual},
		{"bob@Other.example", "sent"},
		{addrReal, sourceManual},
	} {
		require.NoError(t, db.AddAddress(t.Context(), entry.address, entry.source))
	}

	return db
}

func addresses(list []store.Address) []string {
	out := make([]string, len(list))
	for i, entry := range list {
		out[i] = entry.Address
	}

	return out
}

func TestQueryAddresses(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		query store.AddressQuery
		want  []string
		total int
	}{
		{
			"by address by default",
			store.AddressQuery{},
			[]string{
				addrReal,
				addrAlice,
				addrBob,
				addrCarol,
			},
			4,
		},
		{
			"descending",
			store.AddressQuery{Sort: store.SortAddress, Desc: true},
			[]string{
				addrCarol,
				addrBob,
				addrAlice,
				addrReal,
			},
			4,
		},
		{
			"search ignores case",
			store.AddressQuery{Search: "OTHER"},
			[]string{addrBob},
			1,
		},
		{
			"search by source",
			store.AddressQuery{Search: sourceManual},
			[]string{addrReal, addrAlice},
			2,
		},
		{
			"wildcards are literal",
			store.AddressQuery{Search: "%_r"},
			[]string{addrReal},
			1,
		},
		{
			"by source",
			store.AddressQuery{Sort: store.SortSource, Limit: 2},
			[]string{addrReal, addrAlice},
			4,
		},
		{
			"an unknown sort falls back to address",
			store.AddressQuery{Sort: "x; DROP TABLE addresses", Limit: 1},
			[]string{addrReal},
			4,
		},
		{
			"a page",
			store.AddressQuery{Limit: 2, Offset: 2},
			[]string{addrBob, addrCarol},
			4,
		},
		{"past the end", store.AddressQuery{Offset: 9}, nil, 4},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			got, total, err := addressBook(t).QueryAddresses(t.Context(), tt.query)
			require.NoError(t, err)
			require.Equal(t, tt.total, total)

			if tt.want == nil {
				require.Empty(t, got)

				return
			}

			require.Equal(t, tt.want, addresses(got))
		})
	}
}

func TestQueryAddressesHasEverySortKey(t *testing.T) {
	t.Parallel()

	db := addressBook(t)

	for _, sort := range []string{store.SortAddress, store.SortSource, store.SortAdded} {
		for _, desc := range []bool{true, false} {
			got, _, err := db.QueryAddresses(
				t.Context(),
				store.AddressQuery{Sort: sort, Desc: desc},
			)
			require.NoError(t, err, sort)
			require.Len(t, got, 4, sort)
		}
	}
}

func TestQueryAddressesFailsOnAClosedStore(t *testing.T) {
	t.Parallel()

	db := open(t)
	require.NoError(t, db.Close())

	_, _, err := db.QueryAddresses(t.Context(), store.AddressQuery{})
	require.Error(t, err)
}

func TestQueryDecisionsOrdersTiesByWhenTheyWereRecorded(t *testing.T) {
	t.Parallel()

	db := open(t)
	at := time.Date(2026, 10, 30, 12, 0, 0, 0, time.UTC)

	for _, subject := range []string{subjectFirst, subjectSecond, subjectThird} {
		_, err := db.RecordDecision(t.Context(), store.Decision{
			At: at, Subject: subject, Verdict: verdictGood, Reason: reasonBayes, Action: actionNone,
		})
		require.NoError(t, err)
	}

	newest, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{})
	require.NoError(t, err)
	require.Equal(t, []string{subjectThird, subjectSecond, subjectFirst}, subjects(newest))

	oldest, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{Asc: true})
	require.NoError(t, err)
	require.Equal(t, []string{subjectFirst, subjectSecond, subjectThird}, subjects(oldest))
}

func TestRemoveAddresses(t *testing.T) {
	t.Parallel()

	db := addressBook(t)

	removed, err := db.RemoveAddresses(t.Context(), []string{
		"ALICE@example.com", addrAlice, "bob@Other.example", "nobody@example.com",
	})
	require.NoError(t, err)
	require.Equal(t, 2, removed, "case does not matter, repeats and strangers do not count")

	left, _, err := db.QueryAddresses(t.Context(), store.AddressQuery{})
	require.NoError(t, err)
	require.Equal(t, []string{addrReal, addrCarol}, addresses(left))

	none, err := db.RemoveAddresses(t.Context(), nil)
	require.NoError(t, err)
	require.Zero(t, none)
}

func TestRemoveAddressesInBatches(t *testing.T) {
	t.Parallel()

	db := open(t)
	all := make([]string, 0, 1200)

	for i := range 1200 {
		address := fmt.Sprintf("person%04d@example.org", i)
		all = append(all, address)

		require.NoError(t, db.AddAddress(t.Context(), address, "sent"))
	}

	require.NoError(t, db.AddAddress(t.Context(), addrAlice, sourceManual))

	removed, err := db.RemoveAddresses(t.Context(), all)
	require.NoError(t, err)
	require.Equal(t, 1200, removed)

	left, total, err := db.QueryAddresses(t.Context(), store.AddressQuery{})
	require.NoError(t, err)
	require.Equal(t, 1, total)
	require.Equal(t, []string{addrAlice}, addresses(left))
}

func TestRemoveAddressesMatching(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name, search string
		removed      int
		left         []string
	}{
		{"by source", "manual", 2, []string{addrBob, addrCarol}},
		{"by address, ignoring case", "OTHER", 1, []string{addrReal, addrAlice, addrCarol}},
		{"wildcards are literal", "%_r", 1, []string{addrAlice, addrBob, addrCarol}},
		{"nothing matches", "zzz", 0, []string{addrReal, addrAlice, addrBob, addrCarol}},
		{"empty matches everything", "", 4, nil},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			db := addressBook(t)

			removed, err := db.RemoveAddressesMatching(t.Context(), tt.search)
			require.NoError(t, err)
			require.Equal(t, tt.removed, removed)

			left, _, err := db.QueryAddresses(t.Context(), store.AddressQuery{})
			require.NoError(t, err)

			if tt.left == nil {
				require.Empty(t, left)

				return
			}

			require.Equal(t, tt.left, addresses(left))
		})
	}
}

func TestRemovingAddressesFailsOnAClosedStore(t *testing.T) {
	t.Parallel()

	db := open(t)
	require.NoError(t, db.Close())

	_, err := db.RemoveAddresses(t.Context(), []string{addrAlice})
	require.Error(t, err)

	_, err = db.RemoveAddressesMatching(t.Context(), "x")
	require.Error(t, err)
}

func TestQueryDecisionsByID(t *testing.T) {
	t.Parallel()

	db := journal(t)

	all, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{})
	require.NoError(t, err)

	target := all[2]

	got, total, err := db.QueryDecisions(t.Context(), store.DecisionQuery{ID: target.ID})
	require.NoError(t, err)
	require.Equal(t, 1, total)
	require.Equal(t, []string{target.Subject}, subjects(got))

	none, total, err := db.QueryDecisions(t.Context(), store.DecisionQuery{ID: 9999})
	require.NoError(t, err)
	require.Zero(t, total)
	require.Empty(t, none)

	mismatch, total, err := db.QueryDecisions(
		t.Context(),
		store.DecisionQuery{ID: target.ID, Verdict: "nonsense"},
	)
	require.NoError(t, err)
	require.Zero(t, total, "the other filters still apply")
	require.Empty(t, mismatch)
}

func TestDecisionsKnowWhatTheyCorrected(t *testing.T) {
	t.Parallel()

	db := open(t)
	at := time.Date(2026, 10, 30, 12, 0, 0, 0, time.UTC)

	record := func(messageID, reason string) int64 {
		id, err := db.RecordDecision(t.Context(), store.Decision{
			At:        at,
			MessageID: messageID,
			Subject:   reason,
			Verdict:   verdictGood,
			Reason:    reason,
			Action:    actionNone,
		})
		require.NoError(t, err)

		return id
	}

	// One message: judged twice, corrected, then the second judgement undone
	// (an undo marks what it reversed, then records itself).
	first := record("<m@example>", reasonBayes)
	second := record("<m@example>", reasonBayes)
	correction := record("<m@example>", "correction")
	undo := record("<m@example>", "undo")

	require.NoError(t, db.MarkUndone(t.Context(), second))

	other := record("<other@example>", reasonBayes)
	noID := record("", reasonBayes)
	noIDCorrection := record("", "correction")

	get := func(id int64) store.Decision {
		decision, err := db.Decision(t.Context(), id)
		require.NoError(t, err)

		return decision
	}

	require.Equal(
		t,
		[]int64{first, second},
		get(correction).Corrects,
		"both earlier judgements, in order",
	)
	require.Equal(
		t,
		[]int64{second},
		get(undo).Corrects,
		"an undo names what it reversed, not what was corrected first",
	)
	require.Empty(t, get(first).Corrects, "a plain judgement corrects nothing")
	require.Empty(t, get(other).Corrects)
	require.Empty(t, get(noID).Corrects)
	require.Empty(t, get(noIDCorrection).Corrects, "without a Message-ID nothing links")

	require.Equal(t, undo, get(second).UndoneBy)
	require.Equal(
		t,
		correction,
		get(second).CorrectedBy,
		"corrected first, undone later: the two differ",
	)
	require.Zero(t, get(first).UndoneBy, "never undone")
	require.Zero(t, get(undo).UndoneBy)

	listed, _, err := db.QueryDecisions(t.Context(), store.DecisionQuery{ID: second})
	require.NoError(t, err)
	require.Equal(t, undo, listed[0].UndoneBy, "the paged query knows it too")
}

func TestUndoneWithoutAnUndoEntryHasNoLink(t *testing.T) {
	t.Parallel()

	db := open(t)

	id, err := db.RecordDecision(t.Context(), store.Decision{
		At:        time.Now(),
		MessageID: "<m@example>",
		Verdict:   verdictSpam,
		Reason:    reasonBayes,
		Action:    actionJunk,
	})
	require.NoError(t, err)
	require.NoError(t, db.MarkUndone(t.Context(), id))

	decision, err := db.Decision(t.Context(), id)
	require.NoError(t, err)
	require.True(t, decision.Undone)
	require.Zero(t, decision.UndoneBy)
}

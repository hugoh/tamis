package main

import (
	"context"
	"time"

	"github.com/hugoh/tamis/internal/store"
)

var farFuture = time.Date(2100, 1, 1, 0, 0, 0, 0, time.UTC)

const firstDecisionAt = 1_700_000_000

func decision(offset int, messageID, from, subject, mailbox, verdict, reason, action string,
	score float64, applied bool,
) store.Decision {
	return store.Decision{
		At: time.Unix(int64(firstDecisionAt+offset*60), 0).UTC(), Mailbox: mailbox, UID: uint32(100 + offset),
		MessageID: messageID, From: from, Subject: subject, Score: score, Verdict: verdict,
		Reason: reason, Action: action, Applied: applied,
	}
}

func recordDecisions(ctx context.Context, db *store.Store) error {
	entries := []store.Decision{
		decision(1, "<m1@x>", "spam@shop.example", "Free PRIZE inside", "INBOX", "spam", "bayes", "move:Junk", 0.99, true),
		decision(2, "<m1@x>", "spam@shop.example", "Free PRIZE inside", "INBOX", "good", "correction", "train:good", 0.01, true),
		decision(3, "<m2@x>", "alice@example.org", "Lunch", "INBOX", "uncertain", "bayes", "keyword:$TamisUncertain", 0.5, true),
		decision(4, "<m2@x>", "alice@example.org", "Lunch", "INBOX", "good", "undo", "keyword:-$TamisUncertain", 0.5, true),
		decision(5, "<m3@x>", "billing@shop.example", "Receipt 100%", "INBOX", "good", "rule", "move:Receipts", 0.05, false),
		decision(6, "<m1@x>", "spam@shop.example", "Free PRIZE again", "INBOX", "spam", "bayes", "move:Junk", 0.97, false),
		decision(7, "", "nobody@example.org", "No id", "INBOX", "good", "whitelisted", "none", 0.0, false),
		decision(8, "<m4@x>", "x@odd.example", "Under_score", "Junk", "spam", "provider junk", "train:spam", 0.8, true),
		decision(9, "<m4@x>", "x@odd.example", "Under_score", "INBOX", "good", "correction", "train:good", 0.2, true),
		decision(10, "<m4@x>", "x@odd.example", "Under_score", "INBOX", "spam", "undo", "move:INBOX", 0.2, true),
	}

	for _, entry := range entries {
		if _, err := db.RecordDecision(ctx, entry); err != nil {
			return err
		}
	}

	for _, id := range []int64{3, 8} {
		if err := db.MarkUndone(ctx, id); err != nil {
			return err
		}
	}

	return nil
}

func collect(ctx context.Context, db *store.Store, out *goldenStore) error {
	decisions, err := db.Decisions(ctx, 1000)
	if err != nil {
		return err
	}

	for _, d := range decisions {
		out.Decisions = append(out.Decisions, goldenDecision{
			ID: d.ID, At: d.At.Unix(), Mailbox: d.Mailbox, UID: d.UID, MessageID: d.MessageID,
			From: d.From, Subject: d.Subject, Score: d.Score, Verdict: d.Verdict, Reason: d.Reason,
			Action: d.Action, Applied: d.Applied, Undone: d.Undone, CorrectedBy: d.CorrectedBy,
			UndoneBy: d.UndoneBy, Corrects: d.Corrects,
		})
	}

	addresses, err := db.Addresses(ctx)
	if err != nil {
		return err
	}

	for _, a := range addresses {
		out.Addresses = append(out.Addresses, goldenAddress{a.Address, a.Source, a.AddedAt.Unix()})
	}

	keys, err := db.Passkeys(ctx)
	if err != nil {
		return err
	}

	for _, k := range keys {
		out.Passkeys = append(out.Passkeys, goldenPasskey{
			k.ID, k.CredentialID, k.Name, k.Credential, k.CreatedAt.Unix(), k.LastUsed.Unix(),
		})
	}

	stats, err := db.Stats(ctx)
	if err != nil {
		return err
	}

	out.Stats = map[string]int64{
		"spam": int64(stats.Spam), "good": int64(stats.Good), "tokens": int64(stats.Tokens),
		"addresses": int64(stats.Addresses), "decisions": int64(stats.Decisions),
		"oldest_decision": stats.OldestDecision.Unix(),
	}

	if out.Cursor, _, err = db.Cursor(ctx, "INBOX"); err != nil {
		return err
	}

	if out.ModSeq, _, err = db.ModSeq(ctx, "Junk"); err != nil {
		return err
	}

	if out.Setting, _, err = db.Setting(ctx, "auth_server_id"); err != nil {
		return err
	}

	out.Enrollment = "hash-1"

	return queries(ctx, db, out)
}

func queries(ctx context.Context, db *store.Store, out *goldenStore) error {
	decisionQueries := []goldenQuery{
		{Name: "default"},
		{Name: "id", ID: 5},
		{Name: "spam only", Verdict: "spam"},
		{Name: "search sender", Search: "SHOP.example"},
		{Name: "search percent", Search: "100%"},
		{Name: "search underscore", Search: "r_s"},
		{Name: "search reason", Search: "provider"},
		{Name: "score asc", Sort: store.SortScore, Asc: true},
		{Name: "from desc", Sort: store.SortFrom},
		{Name: "folder asc", Sort: store.SortFolder, Asc: true},
		{Name: "subject", Sort: store.SortSubject, Asc: true},
		{Name: "verdict", Sort: store.SortVerdict},
		{Name: "id asc", Sort: store.SortID, Asc: true},
		{Name: "time asc", Sort: store.SortTime, Asc: true},
		{Name: "page", Limit: 3, Offset: 2},
		{Name: "unknown sort", Sort: "nonsense"},
	}

	for _, q := range decisionQueries {
		found, total, err := db.QueryDecisions(ctx, store.DecisionQuery{
			ID: q.ID, Verdict: q.Verdict, Search: q.Search, Sort: q.Sort, Asc: q.Asc,
			Offset: q.Offset, Limit: q.Limit,
		})
		if err != nil {
			return err
		}

		q.Total = total
		for _, d := range found {
			q.Want = append(q.Want, d.ID)
		}

		out.Queries = append(out.Queries, q)
	}

	addressQueries := []goldenQuery{
		{Name: "all", Addresses: true},
		{Name: "search", Addresses: true, Search: "LISTS"},
		{Name: "search literal", Addresses: true, Search: "100%_"},
		{Name: "source desc", Addresses: true, Sort: store.SortSource, Desc: true},
		{Name: "added", Addresses: true, Sort: store.SortAdded},
		{Name: "page", Addresses: true, Limit: 2, Offset: 1},
	}

	for _, q := range addressQueries {
		found, total, err := db.QueryAddresses(ctx, store.AddressQuery{
			Search: q.Search, Sort: q.Sort, Desc: q.Desc, Offset: q.Offset, Limit: q.Limit,
		})
		if err != nil {
			return err
		}

		q.Total = total
		for _, a := range found {
			q.Want = append(q.Want, a.Address)
		}

		out.Queries = append(out.Queries, q)
	}

	return nil
}

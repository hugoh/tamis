package main

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/store"
)

type goldenDecision struct {
	ID          int64   `json:"id"`
	At          int64   `json:"at"`
	Mailbox     string  `json:"mailbox"`
	UID         uint32  `json:"uid"`
	MessageID   string  `json:"message_id"`
	From        string  `json:"from"`
	Subject     string  `json:"subject"`
	Score       float64 `json:"score"`
	Verdict     string  `json:"verdict"`
	Reason      string  `json:"reason"`
	Action      string  `json:"action"`
	Applied     bool    `json:"applied"`
	Undone      bool    `json:"undone"`
	CorrectedBy int64   `json:"corrected_by"`
	UndoneBy    int64   `json:"undone_by"`
	Corrects    []int64 `json:"corrects"`
}

type goldenAddress struct {
	Address string `json:"address"`
	Source  string `json:"source"`
	AddedAt int64  `json:"added_at"`
}

type goldenPasskey struct {
	ID           int64  `json:"id"`
	CredentialID string `json:"credential_id"`
	Name         string `json:"name"`
	Credential   []byte `json:"credential"`
	CreatedAt    int64  `json:"created_at"`
	LastUsed     int64  `json:"last_used"`
}

type goldenQuery struct {
	Name      string `json:"name"`
	ID        int64  `json:"id"`
	Verdict   string `json:"verdict"`
	Search    string `json:"search"`
	Sort      string `json:"sort"`
	Asc       bool   `json:"asc"`
	Desc      bool   `json:"desc"`
	Offset    int    `json:"offset"`
	Limit     int    `json:"limit"`
	Addresses bool   `json:"addresses"`
	Want      []any  `json:"want"`
	Total     int    `json:"total"`
}

type goldenCounts struct {
	Token string `json:"token"`
	Spam  int    `json:"spam"`
	Good  int    `json:"good"`
}

type goldenStore struct {
	Decisions  []goldenDecision `json:"decisions"`
	Addresses  []goldenAddress  `json:"addresses"`
	Passkeys   []goldenPasskey  `json:"passkeys"`
	Queries    []goldenQuery    `json:"queries"`
	Tokens     []string         `json:"tokens"`
	Counts     []goldenCounts   `json:"counts"`
	TotalSpam  int              `json:"total_spam"`
	TotalGood  int              `json:"total_good"`
	Stats      map[string]int64 `json:"stats"`
	Cursor     store.Cursor     `json:"cursor"`
	ModSeq     store.ModSeq     `json:"mod_seq"`
	Setting    string           `json:"setting"`
	Trained    map[string]int   `json:"trained"`
	Enrollment string           `json:"enrollment"`
}

func runStore(dir string) error {
	ctx := context.Background()
	dbPath := filepath.Join(dir, "state.db")

	for _, suffix := range []string{"", "-wal", "-shm"} {
		if err := os.Remove(dbPath + suffix); err != nil && !os.IsNotExist(err) {
			return err
		}
	}

	db, err := store.Open(ctx, dbPath)
	if err != nil {
		return err
	}

	golden, err := populate(ctx, db)
	if err != nil {
		return err
	}

	if err := db.Close(); err != nil {
		return err
	}

	data, err := json.MarshalIndent(golden, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(filepath.Join(dir, "store.json"), append(data, '\n'), 0o644)
}

func populate(ctx context.Context, db *store.Store) (*goldenStore, error) {
	out := &goldenStore{Trained: map[string]int{}}

	trainings := []struct {
		key    string
		tokens []string
		label  classify.Label
	}{
		{"m1", []string{"free", "prize", "url:shop.example"}, classify.Spam},
		{"m2", []string{"free", "lunch", "from:alice@example.org"}, classify.Good},
		{"m3", []string{"lunch", "thursday"}, classify.Good},
		{"m4", []string{"prize", "winner"}, classify.Spam},
		{"m4", []string{"prize", "winner"}, classify.Good},
		{"m1", []string{"free", "prize", "url:shop.example"}, classify.Spam},
		{"m5", []string{"temp", "temp"}, classify.Spam},
	}
	for _, t := range trainings {
		if _, err := db.Train(ctx, t.key, t.tokens, t.label); err != nil {
			return nil, err
		}
	}

	if _, err := db.Untrain(ctx, "m5"); err != nil {
		return nil, err
	}

	for _, key := range []string{"m1", "m2", "m3", "m4", "m5", "m6"} {
		label, ok, err := db.Trained(ctx, key)
		if err != nil {
			return nil, err
		}

		if ok {
			out.Trained[key] = int(label)
		} else {
			out.Trained[key] = -1
		}
	}

	out.Tokens = []string{"free", "prize", "lunch", "winner", "temp", "unseen", "url:shop.example"}

	model, err := db.Model(ctx, out.Tokens)
	if err != nil {
		return nil, err
	}

	out.TotalSpam, out.TotalGood = model.Totals()
	for _, token := range out.Tokens {
		spam, good := model.Counts(token)
		out.Counts = append(out.Counts, goldenCounts{token, spam, good})
	}

	if err := fill(ctx, db, out); err != nil {
		return nil, err
	}

	return out, nil
}

func fill(ctx context.Context, db *store.Store, out *goldenStore) error {
	for _, a := range [][2]string{
		{"Alice@Example.org", "sent"}, {"bob@lists.example.com", "train"},
		{"100%_real@odd.example", "manual"}, {"carol@shop.example", "Sent"},
	} {
		if err := db.AddAddress(ctx, a[0], a[1]); err != nil {
			return err
		}
	}

	if err := db.AddAddress(ctx, "alice@example.org", "again"); err != nil {
		return err
	}

	if err := db.RemoveAddress(ctx, "BOB@lists.example.com"); err != nil {
		return err
	}

	if err := db.AddAddress(ctx, "bob@lists.example.com", "train"); err != nil {
		return err
	}

	if err := db.SetCursor(ctx, "INBOX", store.Cursor{UIDValidity: 7, LastUID: 42}); err != nil {
		return err
	}

	if err := db.SetModSeq(ctx, "Junk", store.ModSeq{UIDValidity: 3, Highest: 1 << 40}); err != nil {
		return err
	}

	if err := db.SetSetting(ctx, "auth_server_id", "icloud.com"); err != nil {
		return err
	}

	if err := db.AddPasskey(ctx, "cred-1", "laptop", []byte{0, 1, 2, 0xff}); err != nil {
		return err
	}

	if err := db.UpdatePasskey(ctx, "cred-1", []byte{9, 8, 7}); err != nil {
		return err
	}

	if err := db.AddEnrollment(ctx, "hash-1", farFuture); err != nil {
		return err
	}

	if err := recordDecisions(ctx, db); err != nil {
		return err
	}

	return collect(ctx, db, out)
}

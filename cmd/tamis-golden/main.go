// Command tamis-golden writes the expected outputs of the Go classifier and
// auth packages for the corpus in tests/fixtures/mail. The Rust port is tested
// against them; delete this command with the Go sources.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/hugoh/tamis/internal/auth"
	"github.com/hugoh/tamis/internal/classify"
)

type model struct {
	counts      map[string][2]int
	spam, total int
}

func (m *model) Counts(token string) (int, int) {
	c := m.counts[token]

	return c[classify.Spam], c[classify.Good]
}

func (m *model) Totals() (int, int) { return m.spam, m.total - m.spam }

func (m *model) train(tokens []string, label classify.Label) {
	for _, token := range tokens {
		c := m.counts[token]
		c[label]++
		m.counts[token] = c
	}

	m.total++

	if label == classify.Spam {
		m.spam++
	}
}

type clue struct {
	Token string  `json:"token"`
	Prob  float64 `json:"prob"`
}

type scored struct {
	File       string   `json:"file"`
	Tokens     []string `json:"tokens"`
	WithStatus []string `json:"with_status"`
	Score      float64  `json:"score"`
	Clues      []clue   `json:"clues"`
}

type verified struct {
	File     string `json:"file"`
	ServerID string `json:"server_id"`
	Verified bool   `json:"verified"`
}

type tally struct {
	Name   string         `json:"name"`
	Parts  map[string]int `json:"parts"`
	Total  int            `json:"total"`
	Domain string         `json:"domain"`
	Ok     bool           `json:"ok"`
}

type golden struct {
	Own       []string   `json:"own"`
	TrainGood []string   `json:"train_good"`
	TrainSpam []string   `json:"train_spam"`
	Cases     []scored   `json:"cases"`
	Auth      []verified `json:"auth"`
	Tallies   []tally    `json:"tallies"`
}

func main() {
	if len(os.Args) != 3 {
		fmt.Fprintln(os.Stderr, "usage: tamis-golden MAILDIR OUT.json")
		os.Exit(2)
	}

	if err := run(os.Args[1], os.Args[2]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(dir, out string) error {
	files, err := filepath.Glob(filepath.Join(dir, "*.eml"))
	if err != nil {
		return err
	}

	out2 := golden{Own: []string{"me@example.org"}}
	mdl := &model{counts: map[string][2]int{}}
	raw := map[string][]byte{}

	for _, path := range files {
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}

		raw[filepath.Base(path)] = data
	}

	for _, label := range []classify.Label{classify.Good, classify.Spam} {
		prefix := "train-" + label.String() + "-"

		for _, path := range files {
			name := filepath.Base(path)
			if !strings.HasPrefix(name, prefix) {
				continue
			}

			tokens := classify.WithRecipientStatus(classify.Tokenize(raw[name]), mdl, out2.Own)
			mdl.train(tokens, label)

			if label == classify.Good {
				out2.TrainGood = append(out2.TrainGood, name)
			} else {
				out2.TrainSpam = append(out2.TrainSpam, name)
			}
		}
	}

	for _, path := range files {
		name := filepath.Base(path)
		if strings.HasPrefix(name, "auth-") {
			for _, id := range []string{"", "icloud.com"} {
				out2.Auth = append(out2.Auth, verified{name, id, auth.Verified(raw[name], id)})
			}

			continue
		}

		tokens := classify.Tokenize(raw[name])
		withStatus := classify.WithRecipientStatus(tokens, mdl, out2.Own)
		result := classify.Score(mdl, withStatus)
		clues := []clue{}

		for _, c := range result.Clues {
			clues = append(clues, clue{c.Token, c.Prob})
		}

		out2.Cases = append(out2.Cases, scored{name, tokens, withStatus, result.Score, clues})
	}

	out2.Tallies = tallies(raw)

	data, err := json.MarshalIndent(out2, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(out, append(data, '\n'), 0o644)
}

func tallies(raw map[string][]byte) []tally {
	build := func(name string, parts map[string]int) tally {
		var t auth.Tally

		for file, n := range parts {
			for range n {
				t.Add(raw[file])
			}
		}

		domain, ok := t.Infer()

		return tally{name, parts, t.Total(), domain, ok}
	}

	return []tally{
		build("icloud-all", map[string]int{"auth-icloud-multi.eml": 25}),
		build("icloud-nine-in-ten", map[string]int{"auth-icloud-multi.eml": 18, "auth-no-header.eml": 2}),
		build("too-few", map[string]int{"auth-icloud-multi.eml": 19}),
		build("mixed", map[string]int{"auth-icloud-multi.eml": 10, "auth-no-header.eml": 15}),
		build("single-provider", map[string]int{"auth-dmarc-pass.eml": 20}),
	}
}

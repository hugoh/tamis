// Package eval measures how well a tokenizer does on labelled mail, by
// progressive validation: each message is scored by a model that has only
// learnt from the messages before it, then learnt from. That is how tamis
// meets mail, and it makes feature experiments comparable.
//
// A feature under test is a Featurizer. The production tokenizer is
// Baseline, so an experiment can't drift from what tamis does.
package eval

import (
	"cmp"
	"errors"
	"slices"

	"github.com/hugoh/tamis/internal/classify"
)

// Thresholds are tamis's defaults for spam_threshold and good_threshold.
const (
	SpamCut = 0.9
	GoodCut = 0.2
)

// DefaultWarmup is how many leading messages tamis-eval learns without
// scoring: the model knows too little to be judged yet.
const DefaultWarmup = 200

const percent = 100

// ErrOneClass means there was nothing of one class to score, so no ranking
// can be measured.
var ErrOneClass = errors.New("need scored messages of both classes")

// Message is one labelled message.
type Message struct {
	Raw  []byte
	Spam bool
}

// Featurizer turns messages into the tokens a model scores. It may remember
// what it has seen. Tokens must not change its state: Learn does, and runs
// after the message was scored.
type Featurizer interface {
	Tokens(raw []byte) []string
	Learn(raw []byte, spam bool)
}

// Variant is a named featurizer; New returns a fresh one for each run, so
// runs don't share memory.
type Variant struct {
	Name string
	New  func() Featurizer
}

// Options tune a run.
type Options struct {
	// Warmup is how many leading messages are learnt but not scored, while
	// the model knows too little to be judged.
	Warmup int
}

// Result is how a variant did on one run. The rates are percentages.
type Result struct {
	// LossPct is 100·(1−AUC): the chance, in percent, that a random spam
	// message scores below a random good one. Lower is better.
	LossPct float64
	// SpamCaught is the share of spam scoring at least SpamCut.
	SpamCaught float64
	// HamFlagged is the share of good mail scoring at least SpamCut.
	HamFlagged float64
	// HamUnsure is the share of good mail between GoodCut and SpamCut.
	HamUnsure float64
	// Ham and Spam are how many messages of each class were scored.
	Ham, Spam int
}

type tamisTokens struct{}

func (tamisTokens) Tokens(raw []byte) []string { return classify.Tokenize(raw) }
func (tamisTokens) Learn([]byte, bool)         {}

// Baseline is the tokenizer tamis uses today.
func Baseline() Variant {
	return Variant{Name: "baseline (tamis today)", New: func() Featurizer { return tamisTokens{} }}
}

// Run scores messages in order with a model that learns as it goes.
func Run(messages []Message, variant Variant, opts Options) (Result, error) {
	featurizer := variant.New()
	model := newModel()

	var ham, spam []float64

	for i, message := range messages {
		tokens := featurizer.Tokens(message.Raw)

		if i >= opts.Warmup {
			score := classify.Score(model, tokens).Score
			if message.Spam {
				spam = append(spam, score)
			} else {
				ham = append(ham, score)
			}
		}

		label := classify.Good
		if message.Spam {
			label = classify.Spam
		}

		model.train(tokens, label)
		featurizer.Learn(message.Raw, message.Spam)
	}

	if len(ham) == 0 || len(spam) == 0 {
		return Result{}, ErrOneClass
	}

	return Result{
		LossPct:    (1 - AUC(ham, spam)) * percent,
		SpamCaught: share(spam, func(s float64) bool { return s >= SpamCut }),
		HamFlagged: share(ham, func(s float64) bool { return s >= SpamCut }),
		HamUnsure:  share(ham, func(s float64) bool { return s > GoodCut && s < SpamCut }),
		Ham:        len(ham),
		Spam:       len(spam),
	}, nil
}

func share(scores []float64, in func(float64) bool) float64 {
	count := 0

	for _, score := range scores {
		if in(score) {
			count++
		}
	}

	return float64(count) / float64(len(scores)) * percent
}

// AUC is the chance that a random spam score is above a random good one,
// counting ties as half.
func AUC(ham, spam []float64) float64 {
	type scored struct {
		score float64
		spam  bool
	}

	all := make([]scored, 0, len(ham)+len(spam))
	for _, score := range ham {
		all = append(all, scored{score, false})
	}

	for _, score := range spam {
		all = append(all, scored{score, true})
	}

	slices.SortFunc(all, func(a, b scored) int { return cmp.Compare(a.score, b.score) })

	var rankSum float64

	for start := 0; start < len(all); {
		end := start
		for end < len(all) && all[end].score == all[start].score {
			end++
		}

		meanRank := float64(start+end+1) / 2 //nolint:mnd // mean of ranks start+1..end

		for _, one := range all[start:end] {
			if one.spam {
				rankSum += meanRank
			}
		}

		start = end
	}

	spamCount, hamCount := float64(len(spam)), float64(len(ham))

	return (rankSum - spamCount*(spamCount+1)/2) / (spamCount * hamCount)
}

// model is the token statistics a score is computed from, learnt in memory.
type model struct {
	counts     map[string][2]int
	spam, good int
}

func newModel() *model {
	return &model{counts: map[string][2]int{}}
}

func (m *model) Counts(token string) (int, int) {
	c := m.counts[token]

	return c[classify.Spam], c[classify.Good]
}

func (m *model) Totals() (int, int) {
	return m.spam, m.good
}

func (m *model) train(tokens []string, label classify.Label) {
	for _, token := range tokens {
		c := m.counts[token]
		c[label]++
		m.counts[token] = c
	}

	if label == classify.Spam {
		m.spam++
	} else {
		m.good++
	}
}

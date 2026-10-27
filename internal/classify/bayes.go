package classify

import (
	"cmp"
	"math"
	"slices"
)

// Label is a training label.
type Label int

// Training labels.
const (
	Good Label = iota
	Spam
)

// String returns "good" or "spam".
func (l Label) String() string {
	if l == Spam {
		return "spam"
	}

	return "good"
}

// Model is the token statistics a score is computed from: how many trained
// spam and good messages contained each token, and how many of each were
// trained in total.
type Model interface {
	Counts(token string) (spam, good int)
	Totals() (spam, good int)
}

// Tuning from SpamBayes' defaults.
const (
	unknownWordStrength = 0.45
	unknownWordProb     = 0.5
	minProbStrength     = 0.1
	maxDiscriminators   = 150
)

const (
	// fisherFactor: under Fisher's method -2·Σln(p) over n clues follows a
	// chi-square distribution with 2n degrees of freedom.
	fisherFactor = 2
	// scoreSpan maps spamness-goodness, in [-1, 1], onto [0, 1].
	scoreSpan = 2
)

// Clue is a token that contributed to a score, with its spam probability.
type Clue struct {
	Token string
	Prob  float64
}

// Result is a message's spam score in [0, 1] (0.5 means no evidence) and the
// clues behind it, strongest first.
type Result struct {
	Score float64
	Clues []Clue
}

// Score combines the tokens' spam probabilities with Fisher's chi-square
// method. Only the most discriminating tokens count.
func Score(model Model, tokens []string) Result {
	clues := discriminators(model, tokens)
	if len(clues) == 0 {
		return Result{Score: unknownWordProb}
	}

	var logSpam, logGood float64
	for _, clue := range clues {
		logGood += math.Log(clue.Prob)
		logSpam += math.Log(1 - clue.Prob)
	}

	degrees := fisherFactor * len(clues)
	spamness := 1 - chi2Q(-fisherFactor*logSpam, degrees)
	goodness := 1 - chi2Q(-fisherFactor*logGood, degrees)

	return Result{Score: (spamness - goodness + 1) / scoreSpan, Clues: clues}
}

func discriminators(model Model, tokens []string) []Clue {
	totalSpam, totalGood := model.Totals()

	clues := make([]Clue, 0, len(tokens))

	for _, token := range tokens {
		spam, good := model.Counts(token)

		prob := tokenProb(spam, good, totalSpam, totalGood)
		if math.Abs(prob-unknownWordProb) >= minProbStrength {
			clues = append(clues, Clue{Token: token, Prob: prob})
		}
	}

	slices.SortStableFunc(clues, func(a, b Clue) int {
		return cmp.Compare(math.Abs(b.Prob-unknownWordProb), math.Abs(a.Prob-unknownWordProb))
	})

	return clues[:min(len(clues), maxDiscriminators)]
}

// tokenProb is Robinson's smoothed probability that a message containing the
// token is spam, pulled towards 0.5 when the token is rarely seen.
func tokenProb(spam, good, totalSpam, totalGood int) float64 {
	seen := float64(spam + good)
	if seen == 0 {
		return unknownWordProb
	}

	var spamRatio, goodRatio float64
	if totalSpam > 0 {
		spamRatio = float64(spam) / float64(totalSpam)
	}

	if totalGood > 0 {
		goodRatio = float64(good) / float64(totalGood)
	}

	raw := spamRatio / (spamRatio + goodRatio)

	return (unknownWordStrength*unknownWordProb + seen*raw) / (unknownWordStrength + seen)
}

// chi2Q is the probability that a chi-square variable with even degrees of
// freedom exceeds x2.
func chi2Q(x2 float64, degrees int) float64 {
	m := x2 / fisherFactor
	term := math.Exp(-m)
	sum := term

	for i := 1; i < degrees/fisherFactor; i++ {
		term *= m / float64(i)
		sum += term
	}

	return min(sum, 1)
}

// MemoryModel is an in-memory Model that can be trained.
type MemoryModel struct {
	counts    map[string][2]int
	totalSpam int
	totalGood int
}

// NewMemoryModel returns an empty model.
func NewMemoryModel() *MemoryModel {
	return &MemoryModel{counts: map[string][2]int{}}
}

// Counts implements Model.
func (m *MemoryModel) Counts(token string) (int, int) {
	c := m.counts[token]

	return c[Spam], c[Good]
}

// Totals implements Model.
func (m *MemoryModel) Totals() (int, int) {
	return m.totalSpam, m.totalGood
}

// Train counts one message's tokens under label.
func (m *MemoryModel) Train(tokens []string, label Label) {
	m.adjust(tokens, label, 1)
}

// Untrain reverses an earlier Train with the same tokens and label.
func (m *MemoryModel) Untrain(tokens []string, label Label) {
	m.adjust(tokens, label, -1)
}

func (m *MemoryModel) adjust(tokens []string, label Label, delta int) {
	for _, token := range tokens {
		c := m.counts[token]
		c[label] = max(0, c[label]+delta)
		m.counts[token] = c
	}

	if label == Spam {
		m.totalSpam = max(0, m.totalSpam+delta)
	} else {
		m.totalGood = max(0, m.totalGood+delta)
	}
}

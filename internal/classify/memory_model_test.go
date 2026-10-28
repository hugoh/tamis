package classify_test

import "github.com/hugoh/tamis/internal/classify"

// memoryModel is an in-memory classify.Model that can be trained.
type memoryModel struct {
	counts    map[string][2]int
	totalSpam int
	totalGood int
}

func newMemoryModel() *memoryModel {
	return &memoryModel{counts: map[string][2]int{}}
}

func (m *memoryModel) Counts(token string) (int, int) {
	c := m.counts[token]

	return c[classify.Spam], c[classify.Good]
}

func (m *memoryModel) Totals() (int, int) {
	return m.totalSpam, m.totalGood
}

func (m *memoryModel) Train(tokens []string, label classify.Label) {
	for _, token := range tokens {
		c := m.counts[token]
		c[label]++
		m.counts[token] = c
	}

	if label == classify.Spam {
		m.totalSpam++
	} else {
		m.totalGood++
	}
}

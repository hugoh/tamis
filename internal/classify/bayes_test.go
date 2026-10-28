package classify_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/stretchr/testify/require"
)

const (
	spamWord    = "viagra"
	goodWord    = "meeting"
	neutralWord = "click"
)

func trainedModel() *memoryModel {
	model := newMemoryModel()

	for range 20 {
		model.Train([]string{spamWord, "winner", neutralWord}, classify.Spam)
		model.Train([]string{goodWord, "agenda", neutralWord}, classify.Good)
	}

	return model
}

func TestScore(t *testing.T) {
	t.Parallel()

	model := trainedModel()

	tests := []struct {
		name     string
		tokens   []string
		min, max float64
	}{
		{"spammy", []string{spamWord, "winner"}, 0.9, 1},
		{"hammy", []string{goodWord, "agenda"}, 0, 0.1},
		{"neutral token only", []string{neutralWord}, 0.5, 0.5},
		{"unknown tokens", []string{"zebra", "quokka"}, 0.5, 0.5},
		{"no tokens", nil, 0.5, 0.5},
		{"mixed", []string{spamWord, goodWord}, 0.2, 0.8},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			result := classify.Score(model, tt.tokens)

			require.GreaterOrEqual(t, result.Score, tt.min)
			require.LessOrEqual(t, result.Score, tt.max)
		})
	}
}

func TestScoreUntrainedModel(t *testing.T) {
	t.Parallel()

	result := classify.Score(newMemoryModel(), []string{"anything"})

	require.InDelta(t, 0.5, result.Score, 1e-9)
	require.Empty(t, result.Clues)
}

func TestScoreReportsStrongestClues(t *testing.T) {
	t.Parallel()

	result := classify.Score(trainedModel(), []string{spamWord, goodWord, neutralWord, "zebra"})

	tokens := make([]string, 0, len(result.Clues))
	for _, clue := range result.Clues {
		tokens = append(tokens, clue.Token)
	}

	require.ElementsMatch(t, []string{spamWord, goodWord}, tokens)
}

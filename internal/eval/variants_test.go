package eval_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/eval"
	"github.com/stretchr/testify/require"
)

func TestStandardStartsWithTheBaseline(t *testing.T) {
	t.Parallel()

	variants := eval.Standard([]string{"me@example.com"})

	require.Len(t, variants, 2)
	require.Equal(t, eval.Baseline(nil).Name, variants[0].Name)
	require.NotEqual(t, variants[0].Name, variants[1].Name)
}

type emptyModel struct{}

func (emptyModel) Counts(string) (int, int) { return 0, 0 }
func (emptyModel) Totals() (int, int)       { return 0, 0 }

func TestFeaturizersIgnoreLearning(t *testing.T) {
	t.Parallel()

	raw := []byte("From: a@x.example\r\nTo: me@example.com\r\nSubject: hi\r\n\r\nhello\r\n")

	for _, variant := range eval.Standard(nil) {
		t.Run(variant.Name, func(t *testing.T) {
			t.Parallel()

			featurizer := variant.New()
			before := featurizer.Tokens(raw, emptyModel{})

			featurizer.Learn(raw, true)
			require.Equal(t, before, featurizer.Tokens(raw, emptyModel{}))
		})
	}
}

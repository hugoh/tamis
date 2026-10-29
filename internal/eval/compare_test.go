package eval_test

import (
	"bytes"
	"testing"

	"github.com/hugoh/tamis/internal/eval"
	"github.com/stretchr/testify/require"
)

func TestCompareIsDeterministicAndPaired(t *testing.T) {
	t.Parallel()

	variants := []eval.Variant{
		eval.Baseline(nil),
		eval.WithoutRecipients(),
	}
	messages := corpus(120)
	options := eval.Options{Warmup: 20}

	first, err := eval.Compare(messages, variants, 3, options)
	require.NoError(t, err)

	second, err := eval.Compare(messages, variants, 3, options)
	require.NoError(t, err)

	require.Equal(t, first, second, "same input, same numbers")
	require.Len(t, first, len(variants))
	require.Len(t, first[0], 3, "one result per run")
}

func TestWriteReport(t *testing.T) {
	t.Parallel()

	variants := []eval.Variant{
		eval.Baseline(nil),
		eval.WithoutRecipients(),
	}

	results, err := eval.Compare(corpus(120), variants, 3, eval.Options{Warmup: 20})
	require.NoError(t, err)

	var out bytes.Buffer

	require.NoError(t, eval.WriteReport(&out, variants, results))
	require.Contains(t, out.String(), "baseline")
	require.Contains(t, out.String(), "recipient tokens")
	require.Contains(t, out.String(), "better in", "paired comparison against the first variant")
}

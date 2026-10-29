package eval

import (
	"fmt"
	"io"
	"math/rand/v2"
	"slices"
	"sync"
	"text/tabwriter"
)

const (
	reportPadding = 2
	shuffleStream = 99
)

// Compare runs every variant over the messages several times. Run 0 keeps
// the order given; the others shuffle it, the same way for every variant, so
// the runs are paired and show how much a difference is noise. Results are
// indexed [variant][run].
func Compare(
	messages []Message,
	variants []Variant,
	runs int,
	opts Options,
) ([][]Result, error) {
	results := make([][]Result, len(variants))
	for i := range results {
		results[i] = make([]Result, runs)
	}

	var (
		group   sync.WaitGroup
		lock    sync.Mutex
		firstEr error
	)

	for run := range runs {
		ordered := messages

		if run > 0 {
			ordered = shuffled(messages, uint64(run))
		}

		for index, variant := range variants {
			group.Go(func() {
				result, err := Run(ordered, variant, opts)

				lock.Lock()
				defer lock.Unlock()

				if err != nil && firstEr == nil {
					firstEr = fmt.Errorf("%s: %w", variant.Name, err)
				}

				results[index][run] = result
			})
		}
	}

	group.Wait()

	return results, firstEr
}

// shuffled returns the messages in a random order that repeats for a seed.
func shuffled(messages []Message, seed uint64) []Message {
	source := rand.New(rand.NewPCG(seed, shuffleStream)) //nolint:gosec // repeatable, not security
	order := source.Perm(len(messages))

	out := make([]Message, len(messages))

	for i, j := range order {
		out[i] = messages[j]
	}

	return out
}

// WriteReport prints one row per variant: the ranking loss on the run in the
// given order and on average, the change from the first variant, and the
// rates at tamis's thresholds on the run in the given order.
func WriteReport(out io.Writer, variants []Variant, results [][]Result) error {
	table := tabwriter.NewWriter(out, 0, 0, reportPadding, ' ', 0)

	_, _ = fmt.Fprintln(
		table,
		"variant\tchrono (1-AUC)%\tmean\tvs first: mean (best..worst)\tspam>=0.9 %\tham>=0.9 %\tham unsure %",
	)

	for index, variant := range variants {
		runs := results[index]
		change := "-"

		if index > 0 {
			change = pairedChange(results[0], runs)
		}

		_, _ = fmt.Fprintf(table, "%s\t%.3f\t%.3f\t%s\t%.1f\t%.2f\t%.1f\n", variant.Name,
			runs[0].LossPct, meanLoss(runs), change,
			runs[0].SpamCaught, runs[0].HamFlagged, runs[0].HamUnsure)
	}

	if err := table.Flush(); err != nil {
		return fmt.Errorf("write report: %w", err)
	}

	return nil
}

func meanLoss(runs []Result) float64 {
	sum := 0.0
	for _, run := range runs {
		sum += run.LossPct
	}

	return sum / float64(len(runs))
}

// pairedChange compares a variant with the baseline run by run, so the noise
// shared by both cancels.
func pairedChange(baseline, runs []Result) string {
	diffs := make([]float64, len(runs))
	better := 0

	for i := range runs {
		diffs[i] = runs[i].LossPct - baseline[i].LossPct
		if diffs[i] < 0 {
			better++
		}
	}

	sum := 0.0
	for _, diff := range diffs {
		sum += diff
	}

	return fmt.Sprintf("%+.3f (%+.3f..%+.3f), better in %d of %d",
		sum/float64(len(diffs)), slices.Min(diffs), slices.Max(diffs), better, len(diffs))
}

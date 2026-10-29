package eval_test

import (
	"fmt"
	"math"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/classify"
	"github.com/hugoh/tamis/internal/eval"
	"github.com/stretchr/testify/require"
)

const (
	spamBody = "Congratulations winner, claim your free lottery prize now"
	hamBody  = "Agenda for the project meeting with the team tomorrow"
)

func mail(from, to, subject, body string, at time.Time) []byte {
	return fmt.Appendf(nil,
		"From: %s\r\nTo: %s\r\nSubject: %s\r\nDate: %s\r\nMessage-ID: <%d@test>\r\n\r\n%s\r\n",
		from, to, subject, at.Format(time.RFC1123Z), at.UnixNano(), body)
}

// corpus alternates one spam and two ham messages, n in all.
func corpus(n int) []eval.Message {
	start := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	out := make([]eval.Message, n)

	for i := range out {
		at := start.Add(time.Duration(i) * time.Hour)

		if i%3 == 0 {
			out[i] = eval.Message{
				Spam: true,
				Raw:  mail("s@spam.example", "me@example.com", "WINNER prize", spamBody, at),
			}
		} else {
			out[i] = eval.Message{
				Raw: mail("f@good.example", "me@example.com", "Project meeting", hamBody, at),
			}
		}
	}

	return out
}

func TestAUC(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name      string
		ham, spam []float64
		want      float64
	}{
		{"perfect", []float64{0.1, 0.2}, []float64{0.8, 0.9}, 1},
		{"reversed", []float64{0.8, 0.9}, []float64{0.1, 0.2}, 0},
		{"all tied", []float64{0.5, 0.5}, []float64{0.5, 0.5}, 0.5},
		{"one pair out of four", []float64{0.1, 0.7}, []float64{0.6, 0.9}, 0.75},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			require.InDelta(t, tt.want, eval.AUC(tt.ham, tt.spam), 1e-12)
		})
	}
}

func TestRunSeparatesTheClasses(t *testing.T) {
	t.Parallel()

	result, err := eval.Run(corpus(150), eval.Baseline(nil), eval.Options{Warmup: 20})
	require.NoError(t, err)

	require.Equal(t, 130, result.Ham+result.Spam, "the warm-up isn't scored")
	require.Less(t, result.LossPct, 1.0)
	require.Greater(t, result.SpamCaught, 80.0)
	require.Zero(t, result.HamFlagged)
}

// spy records the order of calls.
type spy struct {
	events []string
}

func (s *spy) Tokens(raw []byte, _ classify.Model) []string {
	s.events = append(s.events, "tokens:"+string(raw))

	return []string{"t" + string(raw)}
}

func (s *spy) Learn(raw []byte, spam bool) {
	s.events = append(s.events, fmt.Sprintf("learn:%s:%v", raw, spam))
}

func TestRunScoresBeforeLearning(t *testing.T) {
	t.Parallel()

	probe := &spy{}
	variant := eval.Variant{Name: "spy", New: func() eval.Featurizer { return probe }}
	messages := []eval.Message{
		{Raw: []byte("a")},
		{Raw: []byte("b"), Spam: true},
		{Raw: []byte("c")},
	}

	_, err := eval.Run(messages, variant, eval.Options{Warmup: 0})
	require.NoError(t, err)
	require.Equal(t, []string{
		"tokens:a", "learn:a:false",
		"tokens:b", "learn:b:true",
		"tokens:c", "learn:c:false",
	}, probe.events, "each message is scored before it is learnt, and in order")
}

func TestRunNeedsBothClasses(t *testing.T) {
	t.Parallel()

	only := []eval.Message{{Raw: []byte("a")}, {Raw: []byte("b")}}

	_, err := eval.Run(only, eval.Baseline(nil), eval.Options{})
	require.ErrorIs(t, err, eval.ErrOneClass)

	_, err = eval.Run(corpus(30), eval.Baseline(nil), eval.Options{Warmup: 30})
	require.ErrorIs(t, err, eval.ErrOneClass, "nothing left to score after the warm-up")
}

func TestThresholdsMatchTamisDefaults(t *testing.T) {
	t.Parallel()

	require.InDelta(t, 0.9, eval.SpamCut, 1e-12)
	require.InDelta(t, 0.2, eval.GoodCut, 1e-12)
	require.False(t, math.IsNaN(eval.AUC([]float64{0.1}, []float64{0.9})))
	require.Equal(t, "spam", classify.Spam.String())
}

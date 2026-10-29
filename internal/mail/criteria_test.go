package mail

import (
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/stretchr/testify/require"
)

func TestKeywordCriteriaChangedSince(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name         string
		changedSince uint64
		want         *imap.SearchCriteriaModSeq
	}{
		{"no bound", 0, nil},
		{"after a saved mod-sequence", 41, &imap.SearchCriteriaModSeq{ModSeq: 42}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			criteria := keywordCriteria([]string{"$Junk", "Junk"}, time.Time{}, tt.changedSince)
			require.Equal(t, tt.want, criteria.ModSeq)
		})
	}
}

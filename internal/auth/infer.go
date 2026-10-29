package auth

import (
	"bufio"
	"bytes"
	"strings"

	"github.com/emersion/go-message/textproto"
	"github.com/emersion/go-msgauth/authres"
	"golang.org/x/net/publicsuffix"
)

const (
	// minSample is how many messages it takes to tell a provider from noise.
	minSample = 20
	// The provider's domain must head nine in ten messages.
	shareNumerator, shareDenominator = 9, 10
)

// Tally counts, over a sample of messages, the registered domain of the
// authserv-id on each message's topmost Authentication-Results header. The
// provider prepends its own header to everything it delivers, so its domain
// heads nearly all of them; a header a sender forged heads only the few it
// was written for.
type Tally struct {
	total   int
	domains map[string]int
}

// Add counts one message. Messages with no usable header still count towards
// the total, which keeps a sample full of unauthenticated mail from naming a
// provider.
func (t *Tally) Add(raw []byte) {
	t.total++

	domain, ok := topDomain(raw)
	if !ok {
		return
	}

	if t.domains == nil {
		t.domains = map[string]int{}
	}

	t.domains[domain]++
}

// Total is how many messages were counted.
func (t *Tally) Total() int {
	return t.total
}

// Infer returns the provider's domain, if one heads enough of the sample.
func (t *Tally) Infer() (string, bool) {
	if t.total < minSample {
		return "", false
	}

	for domain, count := range t.domains {
		if count*shareDenominator >= t.total*shareNumerator {
			return domain, true
		}
	}

	return "", false
}

func topDomain(raw []byte) (string, bool) {
	header, err := textproto.ReadHeader(bufio.NewReader(bytes.NewReader(raw)))
	if err != nil {
		return "", false
	}

	value := header.Get("Authentication-Results")
	if value == "" {
		return "", false
	}

	authservID, _, err := authres.Parse(stripComments(value))
	if err != nil {
		return "", false
	}

	domain, err := publicsuffix.EffectiveTLDPlusOne(strings.ToLower(authservID))
	if err != nil {
		return "", false
	}

	return domain, true
}

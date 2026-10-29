package classify

import (
	"slices"
	"strings"
)

const (
	// maxRecipients caps the to: tokens of one message, so a mail to a big
	// group doesn't flood the model.
	maxRecipients = 10
	// minSeenRecipient is how many good messages an address must have been a
	// recipient of to count as one mail is expected on.
	minSeenRecipient = 3

	recipientPrefix = "to:"

	statusMine  = "rcpt:mine"
	statusOther = "rcpt:other"
	statusNone  = "rcpt:none"
)

// RecipientStatusTokens lists every token WithRecipientStatus can add, so a
// model can be asked about them before the status is known.
func RecipientStatusTokens() []string {
	return []string{statusMine, statusOther, statusNone}
}

// WithRecipientStatus returns tokens plus one rcpt: token saying whether the
// message went to an address mail is expected on: one of own, or one that at
// least minSeenRecipient good messages were sent to. Spam doesn't teach what
// to expect, so a spammer's target address stays unexpected. A message with
// no To or Cc addresses is rcpt:none.
//
// shortcut: counts since the model began, with no memory of when; an alias
// you retire stays expected. Upgrade by keeping recent counts.
func WithRecipientStatus(tokens []string, model Model, own []string) []string {
	status := statusNone

	for _, token := range tokens {
		address, isRecipient := strings.CutPrefix(token, recipientPrefix)
		if !isRecipient {
			continue
		}

		status = statusOther

		if expected(address, model, own) {
			status = statusMine

			break
		}
	}

	return append(slices.Clone(tokens), status)
}

func expected(address string, model Model, own []string) bool {
	if slices.ContainsFunc(
		own,
		func(mine string) bool { return strings.EqualFold(mine, address) },
	) {
		return true
	}

	_, good := model.Counts(recipientPrefix + address)

	return good >= minSeenRecipient
}

package eval

import (
	"bytes"
	"slices"
	"strings"

	"github.com/emersion/go-message"
	"github.com/emersion/go-message/mail"
	"github.com/hugoh/tamis/internal/classify"
)

// Defaults for RecipientOptions.
const (
	defaultWindow       = 1500
	defaultMinSeen      = 3
	defaultMaxAddresses = 10
)

// RecipientOptions choose which recipient features to add to tamis's tokens.
type RecipientOptions struct {
	// Status adds rcpt:mine, rcpt:other or rcpt:none: whether some To/Cc
	// address is one mail is expected on.
	Status bool
	// PerAddress adds to:<address> and to-domain:<domain> for each To/Cc
	// address, up to MaxAddresses.
	PerAddress bool
	// Own are addresses that are always expected, such as the account's.
	Own []string
	// An address is expected once it has been a recipient of MinSeen of the
	// last Window good messages (defaults 3 and 1500).
	MinSeen, Window int
	// MaxAddresses caps per-address tokens, so a mail to a big group doesn't
	// flood the model (default 10).
	MaxAddresses int
}

// Recipients adds the options' recipient features to tamis's tokens. Only
// good mail teaches it which addresses are expected, so spam can't make an
// address look normal.
func Recipients(options RecipientOptions) Variant {
	options.Window = cmpOr(options.Window, defaultWindow)
	options.MinSeen = cmpOr(options.MinSeen, defaultMinSeen)
	options.MaxAddresses = cmpOr(options.MaxAddresses, defaultMaxAddresses)

	var names []string
	if options.Status {
		names = append(names, "rcpt")
	}

	if options.PerAddress {
		names = append(names, "per-address to:")
	}

	own := make([]string, len(options.Own))
	for i, address := range options.Own {
		own[i] = strings.ToLower(address)
	}

	options.Own = own

	return Variant{
		Name: "+ " + strings.Join(names, " + "),
		New: func() Featurizer {
			return &recipients{options: options, counts: map[string]int{}}
		},
	}
}

func cmpOr(value, fallback int) int {
	if value == 0 {
		return fallback
	}

	return value
}

type recipients struct {
	options RecipientOptions
	ring    [][]string
	counts  map[string]int
}

func (r *recipients) Tokens(raw []byte) []string {
	tokens := classify.Tokenize(raw)
	addresses := recipientsOf(raw)

	if r.options.Status {
		tokens = append(tokens, "rcpt:"+r.status(addresses))
	}

	if r.options.PerAddress {
		for _, address := range addresses[:min(len(addresses), r.options.MaxAddresses)] {
			tokens = append(tokens, "to:"+address)

			if _, domain, ok := strings.Cut(address, "@"); ok {
				tokens = append(tokens, "to-domain:"+domain)
			}
		}
	}

	return tokens
}

func (r *recipients) Learn(raw []byte, spam bool) {
	if spam {
		return
	}

	addresses := recipientsOf(raw)
	r.ring = append(r.ring, addresses)

	for _, address := range addresses {
		r.counts[address]++
	}

	if len(r.ring) > r.options.Window {
		for _, address := range r.ring[0] {
			r.counts[address]--
		}

		r.ring = r.ring[1:]
	}
}

func (r *recipients) status(addresses []string) string {
	switch {
	case len(addresses) == 0:
		return "none"
	case slices.ContainsFunc(addresses, r.expected):
		return "mine"
	default:
		return "other"
	}
}

func (r *recipients) expected(address string) bool {
	return slices.Contains(r.options.Own, address) || r.counts[address] >= r.options.MinSeen
}

// recipientsOf lists the distinct, lower-cased To and Cc addresses.
func recipientsOf(raw []byte) []string {
	entity, err := message.Read(bytes.NewReader(raw))
	if err != nil && entity == nil {
		return nil
	}

	header := mail.Header{Header: entity.Header}

	var out []string

	for _, field := range []string{"To", "Cc"} {
		addresses, _ := header.AddressList(field)
		for _, one := range addresses {
			address := strings.ToLower(one.Address)
			if address != "" && !slices.Contains(out, address) {
				out = append(out, address)
			}
		}
	}

	return out
}

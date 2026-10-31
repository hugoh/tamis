// Package classify scores mail as spam or good with a personal token Bayes
// model (Robinson/Fisher chi-square combining, as in SpamBayes).
package classify

import (
	"bytes"
	"fmt"
	"io"
	"net/netip"
	"net/url"
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/emersion/go-message"
	// Registers non-UTF-8 charsets so real-world mail decodes.
	_ "github.com/emersion/go-message/charset"
	"github.com/emersion/go-message/mail"
	"golang.org/x/net/html"
	"golang.org/x/net/publicsuffix"
)

const (
	minWordLen   = 3
	maxWordLen   = 12
	maxPartBytes = 64 << 10
	skipBucket   = 10
)

var urlPattern = regexp.MustCompile(`(?i)https?://[^\s"'<>()]+`)

// Tokenize extracts the distinct features of a raw RFC 5322 message: words
// from the subject and text parts, sender address and domain, recipient
// addresses and domains, and link hosts.
// It never fails; unparsable input yields whatever could be recovered.
func Tokenize(raw []byte) []string {
	tokens := newTokenSet()

	entity, err := message.Read(bytes.NewReader(raw))
	if err != nil && entity == nil {
		tokens.addText("", string(raw))

		return tokens.list
	}

	header := mail.Header{Header: entity.Header}
	addHeaderTokens(tokens, header)

	_ = entity.Walk(func(_ []int, part *message.Entity, err error) error {
		if err == nil {
			addPartTokens(tokens, part)
		}

		return nil
	})

	return tokens.list
}

func addHeaderTokens(tokens *tokenSet, header mail.Header) {
	if subject, err := header.Subject(); err == nil {
		tokens.addText("subject:", subject)
	}

	addRecipientTokens(tokens, header)

	from, err := header.AddressList("From")
	if err != nil {
		return
	}

	for _, addr := range from {
		address := strings.ToLower(addr.Address)
		tokens.add("from:" + address)

		if _, domain, ok := strings.Cut(address, "@"); ok {
			tokens.add("from-domain:" + domain)
		}
	}
}

// addRecipientTokens records who the message went to: each distinct To and Cc
// address and its domain, up to maxRecipients addresses.
func addRecipientTokens(tokens *tokenSet, header mail.Header) {
	seen := map[string]bool{}

	for _, field := range []string{"To", "Cc"} {
		addresses, _ := header.AddressList(field)

		for _, one := range addresses {
			address := strings.ToLower(one.Address)
			if address == "" || seen[address] {
				continue
			}

			seen[address] = true
			if len(seen) > maxRecipients {
				return
			}

			tokens.add(recipientPrefix + address)

			if _, domain, ok := strings.Cut(address, "@"); ok {
				tokens.add("to-domain:" + domain)
			}
		}
	}
}

func addPartTokens(tokens *tokenSet, part *message.Entity) {
	mediaType, _, err := part.Header.ContentType()
	if err != nil {
		mediaType = "text/plain"
	}

	if mediaType != "text/plain" && mediaType != "text/html" {
		return
	}

	body, err := io.ReadAll(io.LimitReader(part.Body, maxPartBytes))
	if err != nil && len(body) == 0 {
		return
	}

	text := string(body)
	if mediaType == "text/html" {
		text = stripHTML(text)
	}

	tokens.addText("", text)
}

func stripHTML(text string) string {
	var visible, links strings.Builder

	hidden := 0
	tokenizer := html.NewTokenizer(strings.NewReader(text))

	for {
		kind := tokenizer.Next()

		switch kind {
		case html.ErrorToken:
			return visible.String() + links.String()
		case html.TextToken:
			if hidden == 0 {
				visible.WriteString(tokenizer.Token().Data)
			}
		case html.StartTagToken, html.SelfClosingTagToken, html.EndTagToken:
			name, hasAttr := tokenizer.TagName()
			hidden += hiddenDelta(kind, string(name))

			for hasAttr {
				var key, value []byte

				key, value, hasAttr = tokenizer.TagAttr()
				if string(key) == "href" {
					links.WriteString(" " + string(value))
				}
			}

			visible.WriteByte(' ')
		case html.CommentToken, html.DoctypeToken:
		}
	}
}

func hiddenDelta(kind html.TokenType, tag string) int {
	switch {
	case tag != "script" && tag != "style":
		return 0
	case kind == html.StartTagToken:
		return 1
	case kind == html.EndTagToken:
		return -1
	}

	return 0
}

type tokenSet struct {
	seen map[string]struct{}
	list []string
}

func newTokenSet() *tokenSet {
	return &tokenSet{seen: map[string]struct{}{}, list: []string{}}
}

func (s *tokenSet) add(token string) {
	if _, ok := s.seen[token]; ok {
		return
	}

	s.seen[token] = struct{}{}
	s.list = append(s.list, token)
}

func (s *tokenSet) addText(prefix, text string) {
	var rest strings.Builder

	last := 0

	for _, span := range urlPattern.FindAllStringIndex(text, -1) {
		s.addURL(text[span[0]:span[1]])
		rest.WriteString(text[last:span[0]])
		rest.WriteByte(' ')

		last = span[1]
	}

	rest.WriteString(text[last:])

	words := strings.FieldsFunc(strings.ToLower(rest.String()), func(r rune) bool {
		return !unicode.IsLetter(r) && !unicode.IsDigit(r)
	})

	for _, word := range words {
		length := utf8.RuneCountInString(word)

		switch {
		case length < minWordLen:
		case length > maxWordLen:
			first, _ := utf8.DecodeRuneInString(word)
			s.add(fmt.Sprintf("skip:%c %d", first, length/skipBucket*skipBucket))
		default:
			s.add(prefix + word)
		}
	}
}

// addURL records the link's host and each parent domain down to the
// registrable one, so "a.b.example.co.uk" also counts as "example.co.uk".
func (s *tokenSet) addURL(link string) {
	parsed, err := url.Parse(link)
	if err != nil || parsed.Hostname() == "" {
		return
	}

	host := strings.ToLower(parsed.Hostname())
	s.add("url:" + host)

	if _, err := netip.ParseAddr(host); err == nil {
		return
	}

	registrable, err := publicsuffix.EffectiveTLDPlusOne(host)
	if err != nil {
		return
	}

	for host != registrable {
		_, host, _ = strings.Cut(host, ".")
		s.add("url:" + host)
	}
}

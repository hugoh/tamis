// Package classify scores mail as spam or good with a personal token Bayes
// model (Robinson/Fisher chi-square combining, as in SpamBayes).
package classify

import (
	"bytes"
	"fmt"
	"html"
	"io"
	"regexp"
	"strings"
	"unicode"

	"github.com/emersion/go-message"
	// Registers non-UTF-8 charsets so real-world mail decodes.
	_ "github.com/emersion/go-message/charset"
	"github.com/emersion/go-message/mail"
)

const (
	minWordLen   = 3
	maxWordLen   = 12
	maxPartBytes = 64 << 10
	skipBucket   = 10
)

var (
	urlPattern    = regexp.MustCompile(`(?i)https?://[^\s"'<>()]+`)
	hiddenPattern = regexp.MustCompile(`(?is)<(script|style)\b.*?</(script|style)>`)
	tagPattern    = regexp.MustCompile(`(?s)<[^>]*>`)
	hrefPattern   = regexp.MustCompile(`(?i)href\s*=\s*["']?([^"'\s>]+)`)
)

// Tokenize extracts the distinct features of a raw RFC 5322 message: words
// from the subject and text parts, sender address and domain, and link hosts.
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
	text = hiddenPattern.ReplaceAllString(text, " ")

	var links strings.Builder

	for _, match := range hrefPattern.FindAllStringSubmatch(text, -1) {
		links.WriteString(" " + match[1])
	}

	return html.UnescapeString(tagPattern.ReplaceAllString(text, " ")) + links.String()
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
	for _, link := range urlPattern.FindAllString(text, -1) {
		s.addURL(link)
	}

	text = urlPattern.ReplaceAllString(text, " ")

	words := strings.FieldsFunc(strings.ToLower(text), func(r rune) bool {
		return !unicode.IsLetter(r) && !unicode.IsDigit(r)
	})

	for _, word := range words {
		length := len([]rune(word))

		switch {
		case length < minWordLen:
		case length > maxWordLen:
			s.add(fmt.Sprintf("skip:%c %d", []rune(word)[0], length/skipBucket*skipBucket))
		default:
			s.add(prefix + word)
		}
	}
}

// addURL records the link's host and each parent domain down to two labels,
// so "a.b.example.com" also counts as "example.com".
func (s *tokenSet) addURL(link string) {
	host := strings.ToLower(link[strings.Index(link, "://")+3:])
	host, _, _ = strings.Cut(host, "/")
	host, _, _ = strings.Cut(host, "?")
	host, _, _ = strings.Cut(host, ":")

	labels := strings.Split(host, ".")
	for i := 0; i+2 <= len(labels); i++ {
		s.add("url:" + strings.Join(labels[i:], "."))
	}
}

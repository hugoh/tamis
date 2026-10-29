// Package auth decides whether a message's From address is authentic,
// according to the receiving provider's Authentication-Results header
// (RFC 8601).
package auth

import (
	"bufio"
	"bytes"
	"slices"
	"strings"

	"github.com/emersion/go-message"
	"github.com/emersion/go-message/mail"
	"github.com/emersion/go-message/textproto"
	"github.com/emersion/go-msgauth/authres"
)

// Verified reports whether the provider recorded a DMARC pass, or a DKIM
// pass aligned with the From domain. Only the topmost
// Authentication-Results header counts: the provider prepends its own, and
// anything below it may have been written by the sender. If serverID is not
// empty, that header must also name it as its authserv-id: a provider that
// adds no header would otherwise leave a sender-written one on top.
func Verified(raw []byte, serverID string) bool {
	header, err := textproto.ReadHeader(bufio.NewReader(bytes.NewReader(raw)))
	if err != nil {
		return false
	}

	domain, ok := fromDomain(header)
	if !ok {
		return false
	}

	results := header.Values("Authentication-Results")
	if len(results) == 0 {
		return false
	}

	authservID, parsed, err := authres.Parse(stripComments(results[0]))
	if err != nil {
		return false
	}

	if serverID != "" && !strings.EqualFold(authservID, serverID) {
		return false
	}

	return slices.ContainsFunc(parsed, func(result authres.Result) bool {
		return passesFor(result, domain)
	})
}

func fromDomain(header textproto.Header) (string, bool) {
	mailHeader := mail.Header{Header: message.Header{Header: header}}

	from, err := mailHeader.AddressList("From")
	if err != nil || len(from) == 0 {
		return "", false
	}

	_, domain, ok := strings.Cut(strings.ToLower(from[0].Address), "@")

	return domain, ok
}

// stripComments removes RFC 5322 comments, which nest and may hide ";" from a
// parser that does not track the nesting.
func stripComments(header string) string {
	var stripper commentStripper

	for _, char := range header {
		stripper.next(char)
	}

	return stripper.out.String()
}

type commentStripper struct {
	out             strings.Builder
	depth           int
	quoted, escaped bool
}

func (s *commentStripper) keep(char rune) {
	if s.depth == 0 {
		s.out.WriteRune(char)
	}
}

func (s *commentStripper) next(char rune) {
	switch {
	case s.escaped:
		s.escaped = false
		s.keep(char)
	case char == '\\':
		s.escaped = true
		s.keep(char)
	case s.quoted:
		s.quoted = char != '"'
		s.out.WriteRune(char)
	case char == '(':
		s.depth++
	case char == ')' && s.depth > 0:
		s.depth--
		s.keep(' ')
	case s.depth == 0:
		s.quoted = char == '"'
		s.out.WriteRune(char)
	}
}

// passesFor reports whether result authenticates the From domain.
func passesFor(result authres.Result, domain string) bool {
	switch typed := result.(type) {
	case *authres.DMARCResult:
		from := strings.ToLower(typed.From)

		return typed.Value == authres.ResultPass && (from == "" || from == domain)
	case *authres.DKIMResult:
		return typed.Value == authres.ResultPass && aligned(strings.ToLower(typed.Domain), domain)
	default:
		return false
	}
}

// aligned implements DMARC's relaxed alignment: the signing domain is the
// From domain or one of its parents.
func aligned(signer, from string) bool {
	return signer != "" && (from == signer || strings.HasSuffix(from, "."+signer))
}

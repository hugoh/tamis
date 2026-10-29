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
// pass aligned with the From domain.
//
// Without serverID only the topmost Authentication-Results header counts: the
// provider prepends its own, and anything below it may have been written by
// the sender. Some providers (iCloud) prepend several, one per check, each
// with its own authserv-id such as dmarc.icloud.com; serverID then names the
// provider's domain ("icloud.com") and the leading headers whose authserv-id
// is that domain or a subdomain of it count, up to the first one that is not.
// Only the first header per authserv-id counts, so a forged copy of one of the
// provider's ids below the genuine one is ignored. A forged header with an id
// the provider never uses, directly below its block, would still be believed.
func Verified(raw []byte, serverID string) bool {
	header, err := textproto.ReadHeader(bufio.NewReader(bytes.NewReader(raw)))
	if err != nil {
		return false
	}

	domain, ok := fromDomain(header)
	if !ok {
		return false
	}

	seen := map[string]bool{}

	for _, value := range header.Values("Authentication-Results") {
		authservID, parsed, err := authres.Parse(stripComments(value))
		if err != nil || !fromProvider(authservID, serverID) {
			return false
		}

		id := strings.ToLower(authservID)
		if seen[id] {
			continue
		}

		seen[id] = true

		if slices.ContainsFunc(parsed, func(result authres.Result) bool {
			return passesFor(result, domain)
		}) {
			return true
		}

		if serverID == "" {
			return false
		}
	}

	return false
}

// fromProvider reports whether authservID is the configured provider's: its
// domain or a subdomain. Any id will do when none is configured.
func fromProvider(authservID, serverID string) bool {
	if serverID == "" {
		return true
	}

	authservID, serverID = strings.ToLower(authservID), strings.ToLower(serverID)

	return authservID == serverID || strings.HasSuffix(authservID, "."+serverID)
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

// Package auth decides whether a message's From address is authentic,
// according to the receiving provider's Authentication-Results header
// (RFC 8601).
package auth

import (
	"bufio"
	"bytes"
	"regexp"
	"strings"

	"github.com/emersion/go-message"
	"github.com/emersion/go-message/mail"
	"github.com/emersion/go-message/textproto"
)

var comments = regexp.MustCompile(`\([^)]*\)`)

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

	authservID, parsed := parse(results[0])
	if serverID != "" && authservID != strings.ToLower(serverID) {
		return false
	}

	for _, result := range parsed {
		if result.passesFor(domain) {
			return true
		}
	}

	return false
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

type methodResult struct {
	method, value string
	props         map[string]string
}

// passesFor reports whether this result authenticates the From domain.
func (r methodResult) passesFor(domain string) bool {
	if r.value != "pass" {
		return false
	}

	switch r.method {
	case "dmarc":
		headerFrom := r.props["header.from"]

		return headerFrom == "" || headerFrom == domain
	case "dkim":
		return aligned(r.props["header.d"], domain)
	default:
		return false
	}
}

// parse splits "authserv-id; method=result ptype.prop=value ...; ..." into
// the authserv-id and its method results.
func parse(header string) (string, []methodResult) {
	parts := strings.Split(comments.ReplaceAllString(strings.ToLower(header), " "), ";")

	authservID, _, _ := strings.Cut(strings.TrimSpace(parts[0]), " ")
	results := make([]methodResult, 0, len(parts))

	for _, part := range parts[1:] {
		fields := strings.Fields(part)
		if len(fields) == 0 {
			continue
		}

		method, value, ok := strings.Cut(fields[0], "=")
		if !ok {
			continue
		}

		result := methodResult{method: method, value: value, props: map[string]string{}}

		for _, field := range fields[1:] {
			if key, val, ok := strings.Cut(field, "="); ok {
				result.props[key] = val
			}
		}

		results = append(results, result)
	}

	return authservID, results
}

// aligned implements DMARC's relaxed alignment: the signing domain is the
// From domain or one of its parents.
func aligned(signer, from string) bool {
	return signer != "" && (from == signer || strings.HasSuffix(from, "."+signer))
}

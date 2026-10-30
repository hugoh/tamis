// Package web is tamis's web interface.
package web

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
)

const tokenBytes = 32

// NewToken returns a random token for a one-time link.
func NewToken() (string, error) {
	raw := make([]byte, tokenBytes)
	if _, err := rand.Read(raw); err != nil {
		return "", fmt.Errorf("generate token: %w", err)
	}

	return b64(raw), nil
}

// HashToken is what the store keeps of a token, so a copy of the database
// can't be used to enroll a passkey.
func HashToken(token string) string {
	sum := sha256.Sum256([]byte(token))

	return hex.EncodeToString(sum[:])
}

func b64(raw []byte) string { return base64.RawURLEncoding.EncodeToString(raw) }

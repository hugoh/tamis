package web_test

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"testing"

	"github.com/fxamacker/cbor/v2"
	"github.com/stretchr/testify/require"
)

const (
	flagUserPresent  = 0x01
	flagUserVerified = 0x04
	flagAttested     = 0x40
)

func b64(raw []byte) string { return base64.RawURLEncoding.EncodeToString(raw) }

// authenticator is a software passkey: it answers WebAuthn ceremonies the way
// a browser and authenticator would, with "none" attestation.
type authenticator struct {
	t          *testing.T
	origin     string
	rpID       string
	key        *ecdsa.PrivateKey
	credID     []byte
	userHandle []byte
	counter    uint32
}

func newAuthenticator(t *testing.T, origin string) *authenticator {
	t.Helper()

	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	require.NoError(t, err)

	credID := make([]byte, 32)
	_, err = rand.Read(credID)
	require.NoError(t, err)

	return &authenticator{t: t, origin: origin, rpID: "localhost", key: key, credID: credID}
}

type options struct {
	PublicKey struct {
		Challenge string `json:"challenge"`
		User      struct {
			ID string `json:"id"`
		} `json:"user"`
	} `json:"publicKey"`
}

func (a *authenticator) parse(optionsJSON []byte) options {
	a.t.Helper()

	var opts options
	require.NoError(a.t, json.Unmarshal(optionsJSON, &opts), string(optionsJSON))

	return opts
}

func (a *authenticator) clientData(kind, challenge string) []byte {
	a.t.Helper()

	data, err := json.Marshal(map[string]any{
		"type": kind, "challenge": challenge, "origin": a.origin, "crossOrigin": false,
	})
	require.NoError(a.t, err)

	return data
}

func (a *authenticator) authData(flags byte, attested bool) []byte {
	a.t.Helper()

	rpHash := sha256.Sum256([]byte(a.rpID))
	data := append([]byte{}, rpHash[:]...)
	data = append(data, flags)
	data = binary.BigEndian.AppendUint32(data, a.counter)

	if !attested {
		return data
	}

	data = append(data, make([]byte, 16)...)                          // AAGUID
	data = binary.BigEndian.AppendUint16(data, uint16(len(a.credID))) //nolint:gosec // 32 bytes
	data = append(data, a.credID...)

	x := a.key.X.FillBytes(make([]byte, 32))
	y := a.key.Y.FillBytes(make([]byte, 32))

	publicKey, err := cbor.Marshal(map[int]any{1: 2, 3: -7, -1: 1, -2: x, -3: y})
	require.NoError(a.t, err)

	return append(data, publicKey...)
}

// register answers a registration ceremony's options with a credential.
func (a *authenticator) register(optionsJSON []byte) []byte {
	a.t.Helper()

	opts := a.parse(optionsJSON)

	userHandle, err := base64.RawURLEncoding.DecodeString(opts.PublicKey.User.ID)
	require.NoError(a.t, err)

	a.userHandle = userHandle

	attestation, err := cbor.Marshal(map[string]any{
		"fmt":      "none",
		"attStmt":  map[string]any{},
		"authData": a.authData(flagUserPresent|flagUserVerified|flagAttested, true),
	})
	require.NoError(a.t, err)

	return a.body(map[string]any{
		"clientDataJSON": b64(
			a.clientData("webauthn.create", opts.PublicKey.Challenge),
		),
		"attestationObject": b64(attestation),
	})
}

// login answers a login ceremony's options with an assertion.
func (a *authenticator) login(optionsJSON []byte) []byte {
	a.t.Helper()

	opts := a.parse(optionsJSON)
	a.counter++

	authData := a.authData(flagUserPresent|flagUserVerified, false)
	clientData := a.clientData("webauthn.get", opts.PublicKey.Challenge)
	clientHash := sha256.Sum256(clientData)
	signed := sha256.Sum256(append(append([]byte{}, authData...), clientHash[:]...))

	signature, err := ecdsa.SignASN1(rand.Reader, a.key, signed[:])
	require.NoError(a.t, err)

	return a.body(map[string]any{
		"clientDataJSON":    b64(clientData),
		"authenticatorData": b64(authData),
		"signature":         b64(signature),
		"userHandle":        b64(a.userHandle),
	})
}

func (a *authenticator) body(response map[string]any) []byte {
	a.t.Helper()

	id := b64(a.credID)

	body, err := json.Marshal(map[string]any{
		"id": id, "rawId": id, "type": "public-key", "response": response,
	})
	require.NoError(a.t, err)

	return body
}

package web

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/go-webauthn/webauthn/protocol"
	"github.com/go-webauthn/webauthn/webauthn"
	"github.com/hugoh/tamis/internal/store"
)

const (
	maxPasskeyName = 64
	defaultName    = "passkey"
)

const ownerName = "tamis"

var errNotOwner = errors.New("unknown user handle")

func ownerID() []byte { return []byte(ownerName) }

type ceremonyKind int

const (
	registering ceremonyKind = iota + 1
	loggingIn
)

// ceremony is a WebAuthn exchange between its begin and finish requests. The
// kind keeps a login challenge, which anyone can ask for, from completing a
// registration.
type ceremony struct {
	kind ceremonyKind
	data webauthn.SessionData
}

// owner is the one user: every enrolled passkey is theirs.
type owner struct {
	credentials []webauthn.Credential
}

func (o *owner) WebAuthnID() []byte                         { return ownerID() }
func (o *owner) WebAuthnName() string                       { return ownerName }
func (o *owner) WebAuthnDisplayName() string                { return ownerName }
func (o *owner) WebAuthnCredentials() []webauthn.Credential { return o.credentials }

func (s *Server) owner(req *http.Request) (*owner, error) {
	keys, err := s.db.Passkeys(req.Context())
	if err != nil {
		return nil, fmt.Errorf("load passkeys: %w", err)
	}

	credentials := make([]webauthn.Credential, 0, len(keys))

	for _, key := range keys {
		var credential webauthn.Credential
		if err := json.Unmarshal(key.Credential, &credential); err != nil {
			return nil, fmt.Errorf("decode passkey %d: %w", key.ID, err)
		}

		credentials = append(credentials, credential)
	}

	return &owner{credentials: credentials}, nil
}

func (s *Server) setCookie(resp http.ResponseWriter, name, value, path string, ttl time.Duration) {
	http.SetCookie(
		resp,
		&http.Cookie{ //nolint:gosec // Secure follows the configured origin: plain HTTP is for loopback
			Name:     name,
			Value:    value,
			Path:     path,
			MaxAge:   int(ttl.Seconds()),
			HttpOnly: true,
			Secure:   s.secure,
			SameSite: http.SameSiteStrictMode,
		},
	)
}

func (s *Server) clearCookie(resp http.ResponseWriter, name, path string) {
	s.setCookie(resp, name, "", path, -time.Second)
}

func (s *Server) signedIn(req *http.Request) bool {
	cookie, err := req.Cookie(sessionCookie)
	if err != nil {
		return false
	}

	_, ok := s.sessions.get(cookie.Value)

	return ok
}

func (s *Server) startSession(resp http.ResponseWriter) error {
	id, err := s.sessions.put(struct{}{}, sessionTTL)
	if err != nil {
		return err
	}

	s.setCookie(resp, sessionCookie, id, "/", sessionTTL)

	return nil
}

func (s *Server) requireSession(next http.Handler) http.Handler {
	return http.HandlerFunc(func(resp http.ResponseWriter, req *http.Request) {
		if s.signedIn(req) {
			next.ServeHTTP(resp, req)

			return
		}

		if req.Method == http.MethodGet {
			http.Redirect(resp, req, "/auth/login", http.StatusSeeOther)

			return
		}

		http.Error(resp, "sign in first", http.StatusUnauthorized)
	})
}

func (s *Server) beginCeremony(
	resp http.ResponseWriter,
	kind ceremonyKind,
	data *webauthn.SessionData,
	options any,
) {
	ceremonyID, err := s.ceremonies.put(ceremony{kind: kind, data: *data}, ceremonyTTL)
	if err != nil {
		s.fail(resp, "start ceremony", err)

		return
	}

	s.setCookie(resp, ceremonyCookie, ceremonyID, "/auth/", ceremonyTTL)
	resp.Header().Set("Content-Type", "application/json")

	if err := json.NewEncoder(resp).Encode(options); err != nil {
		s.log.Error("write ceremony options", "err", err)
	}
}

// finishCeremony consumes the ceremony the request's cookie names.
func (s *Server) finishCeremony(
	resp http.ResponseWriter,
	req *http.Request,
	kind ceremonyKind,
) (webauthn.SessionData, bool) {
	cookie, err := req.Cookie(ceremonyCookie)
	if err == nil {
		s.clearCookie(resp, ceremonyCookie, "/auth/")

		if found, ok := s.ceremonies.take(cookie.Value); ok && found.kind == kind {
			return found.data, true
		}
	}

	http.Error(resp, "start again", http.StatusUnauthorized)

	return webauthn.SessionData{}, false
}

func (s *Server) fail(resp http.ResponseWriter, what string, err error) {
	s.log.Error(what, "err", err)
	http.Error(resp, "internal error", http.StatusInternalServerError)
}

func (s *Server) rejected(resp http.ResponseWriter, what string, err error) {
	s.log.Warn(what, "err", err)
	http.Error(resp, "not accepted", http.StatusUnauthorized)
}

func (s *Server) registerBegin(resp http.ResponseWriter, req *http.Request) {
	if !s.signedIn(req) {
		used, err := s.db.ConsumeEnrollment(
			req.Context(),
			HashToken(req.URL.Query().Get("token")),
			time.Now(),
		)
		if err != nil {
			s.fail(resp, "check enrollment token", err)

			return
		}

		if !used {
			http.Error(
				resp,
				"this link has expired or was already used; run `tamis web enroll`",
				http.StatusUnauthorized,
			)

			return
		}
	}

	user, err := s.owner(req)
	if err != nil {
		s.fail(resp, "begin registration", err)

		return
	}

	exclude := make([]protocol.CredentialDescriptor, 0, len(user.credentials))
	for _, credential := range user.credentials {
		exclude = append(exclude, credential.Descriptor())
	}

	creation, data, err := s.wa.BeginRegistration(user,
		webauthn.WithResidentKeyRequirement(protocol.ResidentKeyRequirementRequired),
		webauthn.WithExclusions(exclude),
	)
	if err != nil {
		s.fail(resp, "begin registration", err)

		return
	}

	s.beginCeremony(resp, registering, data, creation)
}

func (s *Server) registerFinish(resp http.ResponseWriter, req *http.Request) {
	data, ok := s.finishCeremony(resp, req, registering)
	if !ok {
		return
	}

	user, err := s.owner(req)
	if err != nil {
		s.fail(resp, "finish registration", err)

		return
	}

	credential, err := s.wa.FinishRegistration(user, data, req)
	if err != nil {
		s.rejected(resp, "finish registration", err)

		return
	}

	blob, err := json.Marshal(credential)
	if err != nil {
		s.fail(resp, "encode passkey", err)

		return
	}

	err = s.db.AddPasskey(
		req.Context(),
		b64(credential.ID),
		passkeyName(req.URL.Query().Get("name")),
		blob,
	)
	if errors.Is(err, store.ErrExists) {
		http.Error(resp, "that passkey is already enrolled", http.StatusConflict)

		return
	}

	if err != nil {
		s.fail(resp, "save passkey", err)

		return
	}

	s.signIn(resp)
}

func (s *Server) signIn(resp http.ResponseWriter) {
	if err := s.startSession(resp); err != nil {
		s.fail(resp, "start session", err)

		return
	}

	resp.Header().Set("Content-Type", "application/json")
	_, _ = resp.Write([]byte("{}"))
}

func (s *Server) loginBegin(resp http.ResponseWriter, _ *http.Request) {
	assertion, data, err := s.wa.BeginDiscoverableLogin()
	if err != nil {
		s.fail(resp, "begin login", err)

		return
	}

	s.beginCeremony(resp, loggingIn, data, assertion)
}

func (s *Server) loginFinish(resp http.ResponseWriter, req *http.Request) {
	data, ok := s.finishCeremony(resp, req, loggingIn)
	if !ok {
		return
	}

	_, credential, err := s.wa.FinishPasskeyLogin(
		func(_, userHandle []byte) (webauthn.User, error) {
			if !bytes.Equal(userHandle, ownerID()) {
				return nil, errNotOwner
			}

			return s.owner(req)
		},
		data, req,
	)
	if err != nil {
		s.rejected(resp, "finish login", err)

		return
	}

	blob, err := json.Marshal(credential)
	if err != nil {
		s.fail(resp, "encode passkey", err)

		return
	}

	if err := s.db.UpdatePasskey(req.Context(), b64(credential.ID), blob); err != nil {
		s.fail(resp, "record login", err)

		return
	}

	s.signIn(resp)
}

func (s *Server) logout(resp http.ResponseWriter, req *http.Request) {
	if cookie, err := req.Cookie(sessionCookie); err == nil {
		s.sessions.take(cookie.Value)
	}

	s.clearCookie(resp, sessionCookie, "/")
	http.Redirect(resp, req, "/auth/login", http.StatusSeeOther)
}

// passkeyName tidies a name the user typed.
func passkeyName(name string) string {
	name = strings.TrimSpace(name)

	if utf8.RuneCountInString(name) > maxPasskeyName {
		name = string([]rune(name)[:maxPasskeyName])
	}

	if name == "" {
		return defaultName
	}

	return name
}

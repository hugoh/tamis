// Package imaptest runs an in-memory IMAP server for tests.
package imaptest

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"io"
	"log"
	"math/big"
	"net"
	"testing"
	"time"

	"github.com/emersion/go-imap/v2"
	"github.com/emersion/go-imap/v2/imapserver"
	"github.com/emersion/go-imap/v2/imapserver/imapmemserver"
)

// Test account credentials.
const (
	Username = "user@example.com"
	Password = "app-specific-password" //nolint:gosec // fake test credential
)

const loopback = "127.0.0.1"

// Rev2 advertises IMAP4rev2, which implies MOVE, UIDPLUS and friends.
func Rev2() imap.CapSet {
	return imap.CapSet{imap.CapIMAP4rev1: {}, imap.CapIMAP4rev2: {}}
}

// Rev1 advertises bare IMAP4rev1: no MOVE, no UIDPLUS.
func Rev1() imap.CapSet {
	return imap.CapSet{imap.CapIMAP4rev1: {}}
}

// Options configures a test server.
type Options struct {
	Caps imap.CapSet
	// Fail, if set, is asked before each command; returning true makes the
	// command fail with NO. op is the IMAP command name, mailbox the folder
	// it targets ("" when it has none).
	Fail func(op, mailbox string) bool
	// Drop, if set, is asked before each command like Fail; returning true
	// closes the connection without answering, as a server that gave up on
	// the client does.
	Drop func(op, mailbox string) bool
	// TLS serves implicit TLS with a throwaway certificate.
	TLS bool
}

// Server is a running test server.
type Server struct {
	// Addr is the host:port to dial.
	Addr string
	// TLSConfig trusts the server's certificate; nil for plaintext servers.
	TLSConfig *tls.Config
}

// Start runs a plaintext server with INBOX and Junk folders until the test
// ends.
func Start(tb testing.TB, caps imap.CapSet) *Server {
	tb.Helper()

	return StartWith(tb, Options{Caps: caps})
}

// StartWith runs a server configured by options until the test ends.
func StartWith(tb testing.TB, options Options) *Server {
	tb.Helper()

	listener, err := (&net.ListenConfig{}).Listen(tb.Context(), "tcp", loopback+":0")
	if err != nil {
		tb.Fatalf("listen: %v", err)
	}

	srv := &Server{Addr: listener.Addr().String()}

	if options.TLS {
		cert, pool := selfSigned(tb)
		listener = tls.NewListener(listener, &tls.Config{
			Certificates: []tls.Certificate{cert},
			MinVersion:   tls.VersionTLS12,
		})
		srv.TLSConfig = &tls.Config{
			RootCAs:    pool,
			ServerName: loopback,
			MinVersion: tls.VersionTLS12,
		}
	}

	serve(tb, listener, options)

	return srv
}

func serve(tb testing.TB, listener net.Listener, options Options) {
	tb.Helper()

	mem := imapmemserver.New()
	user := imapmemserver.NewUser(Username, Password)

	for _, name := range []string{"INBOX", "Junk"} {
		if err := user.Create(name, nil); err != nil {
			tb.Fatalf("create %s: %v", name, err)
		}
	}

	mem.AddUser(user)

	server := imapserver.New(&imapserver.Options{
		NewSession: func(conn *imapserver.Conn) (imapserver.Session, *imapserver.GreetingData, error) {
			sess, err := wrap(mem.NewSession(), conn, options.Fail, options.Drop)

			return sess, nil, err
		},
		Caps:         options.Caps,
		InsecureAuth: true,
		Logger:       log.New(io.Discard, "", 0),
	})

	go func() { _ = server.Serve(listener) }()

	tb.Cleanup(func() { _ = server.Close() })
}

func selfSigned(tb testing.TB) (tls.Certificate, *x509.CertPool) {
	tb.Helper()

	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		tb.Fatalf("generate key: %v", err)
	}

	template := &x509.Certificate{
		SerialNumber: big.NewInt(1),
		Subject:      pkix.Name{CommonName: "imaptest"},
		IPAddresses:  []net.IP{net.ParseIP(loopback)},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
	}

	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		tb.Fatalf("create certificate: %v", err)
	}

	parsed, err := x509.ParseCertificate(der)
	if err != nil {
		tb.Fatalf("parse certificate: %v", err)
	}

	pool := x509.NewCertPool()
	pool.AddCert(parsed)

	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, pool
}

package web_test

import (
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

const tlsHost = "minipc.example.net"

func readFile(t *testing.T, path string) []byte {
	t.Helper()

	data, err := os.ReadFile(path) //nolint:gosec // a path the test made
	require.NoError(t, err)

	return data
}

func readCert(t *testing.T, path string) *x509.Certificate {
	t.Helper()

	data := readFile(t, path)

	block, _ := pem.Decode(data)
	require.NotNil(t, block, "no PEM block in %s", path)

	cert, err := x509.ParseCertificate(block.Bytes)
	require.NoError(t, err)

	return cert
}

func TestSelfSignedMakesACertificateBrowsersCanTrust(t *testing.T) {
	t.Parallel()

	dir := filepath.Join(t.TempDir(), "state")
	now := time.Now()

	certFile, keyFile, err := web.SelfSigned(dir, tlsHost, now)
	require.NoError(t, err)

	info, err := os.Stat(keyFile)
	require.NoError(t, err)
	require.Equal(t, os.FileMode(0o600), info.Mode().Perm(), "the key is private")

	cert := readCert(t, certFile)
	require.Contains(t, cert.DNSNames, tlsHost)
	require.Contains(t, cert.DNSNames, "localhost")
	require.Contains(t, cert.ExtKeyUsage, x509.ExtKeyUsageServerAuth)
	require.LessOrEqual(
		t,
		cert.NotAfter.Sub(cert.NotBefore),
		825*24*time.Hour,
		"macOS rejects longer",
	)
	require.Greater(t, cert.NotAfter.Sub(now), 365*24*time.Hour)

	roots := x509.NewCertPool()
	roots.AddCert(cert)

	_, err = cert.Verify(x509.VerifyOptions{
		Roots:       roots,
		DNSName:     tlsHost,
		CurrentTime: now,
		KeyUsages:   []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	})
	require.NoError(t, err, "trusting this certificate is enough for the host")
}

func TestSelfSignedForAnIPAddress(t *testing.T) {
	t.Parallel()

	certFile, _, err := web.SelfSigned(t.TempDir(), "192.168.1.5", time.Now())
	require.NoError(t, err)

	cert := readCert(t, certFile)
	require.NoError(t, cert.VerifyHostname("192.168.1.5"))
}

func TestSelfSignedIsKept(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	now := time.Now()

	certFile, keyFile, err := web.SelfSigned(dir, tlsHost, now)
	require.NoError(t, err)

	first := readFile(t, certFile)

	again, againKey, err := web.SelfSigned(dir, tlsHost, now.Add(24*time.Hour))
	require.NoError(t, err)
	require.Equal(t, certFile, again)
	require.Equal(t, keyFile, againKey)

	second := readFile(t, again)
	require.Equal(t, first, second, "trusting it once is enough")
}

func TestSelfSignedIsMadeAgainWhenItNoLongerFits(t *testing.T) {
	t.Parallel()

	now := time.Now()

	tests := map[string]struct {
		host string
		at   time.Time
	}{
		"another host":        {"other.example.net", now},
		"close to expiry":     {tlsHost, now.Add(800*24*time.Hour - 10*24*time.Hour)},
		"already expired":     {tlsHost, now.Add(900 * 24 * time.Hour)},
		"clock before issued": {tlsHost, now.Add(-48 * time.Hour)},
	}

	for name, tt := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := t.TempDir()

			certFile, _, err := web.SelfSigned(dir, tlsHost, now)
			require.NoError(t, err)

			before := readFile(t, certFile)

			_, _, err = web.SelfSigned(dir, tt.host, tt.at)
			require.NoError(t, err)

			after := readFile(t, certFile)
			require.NotEqual(t, before, after)
			require.NoError(t, readCert(t, certFile).VerifyHostname(tt.host))
		})
	}
}

func TestSelfSignedIgnoresWreckage(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	certFile, keyFile, err := web.SelfSigned(dir, tlsHost, time.Now())
	require.NoError(t, err)
	require.NoError(t, os.WriteFile(certFile, []byte("not a certificate"), 0o600))

	_, _, err = web.SelfSigned(dir, tlsHost, time.Now())
	require.NoError(t, err)
	require.NoError(t, readCert(t, certFile).VerifyHostname(tlsHost))

	_, err = tls.LoadX509KeyPair(certFile, keyFile)
	require.NoError(t, err, "the pair matches")
}

func TestSelfSignedCannotWriteWhereItIsNotAllowed(t *testing.T) {
	t.Parallel()

	file := filepath.Join(t.TempDir(), "file")
	require.NoError(t, os.WriteFile(file, nil, 0o600))

	_, _, err := web.SelfSigned(filepath.Join(file, "under-a-file"), tlsHost, time.Now())
	require.Error(t, err)
}

func TestTLSListenerServesHTTPS(t *testing.T) {
	t.Parallel()

	certFile, keyFile, err := web.SelfSigned(t.TempDir(), tlsHost, time.Now())
	require.NoError(t, err)

	var lc net.ListenConfig

	plain, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)

	listener, err := web.TLSListener(plain, certFile, keyFile)
	require.NoError(t, err)

	server := &http.Server{
		ReadHeaderTimeout: time.Second,
		ErrorLog:          log.New(io.Discard, "", 0),
		Handler: http.HandlerFunc(func(resp http.ResponseWriter, _ *http.Request) {
			_, _ = resp.Write([]byte("hello"))
		}),
	}

	go func() { _ = server.Serve(listener) }()

	t.Cleanup(func() { _ = server.Close() })

	roots := x509.NewCertPool()
	roots.AddCert(readCert(t, certFile))

	get := func(pool *x509.CertPool) error {
		client := &http.Client{Transport: &http.Transport{
			TLSClientConfig: &tls.Config{
				RootCAs:    pool,
				ServerName: tlsHost,
				MinVersion: tls.VersionTLS12,
			},
		}}

		req, err := http.NewRequestWithContext(
			t.Context(),
			http.MethodGet,
			"https://"+plain.Addr().String(),
			nil,
		)
		require.NoError(t, err)

		resp, err := client.Do(req)
		if err != nil {
			return fmt.Errorf("get: %w", err)
		}

		return resp.Body.Close()
	}

	require.NoError(t, get(roots), "a client that trusts the certificate connects")
	require.Error(t, get(x509.NewCertPool()), "one that doesn't, doesn't")
}

func TestTLSListenerNeedsTheFiles(t *testing.T) {
	t.Parallel()

	var lc net.ListenConfig

	plain, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)
	t.Cleanup(func() { _ = plain.Close() })

	_, err = web.TLSListener(plain, "/nonexistent/cert.pem", "/nonexistent/key.pem")
	require.Error(t, err)
}

func TestFingerprint(t *testing.T) {
	t.Parallel()

	certFile, _, err := web.SelfSigned(t.TempDir(), tlsHost, time.Now())
	require.NoError(t, err)

	got, err := web.Fingerprint(certFile)
	require.NoError(t, err)

	sum := sha256.Sum256(readCert(t, certFile).Raw)
	parts := make([]string, len(sum))

	for i, b := range sum {
		parts[i] = fmt.Sprintf("%02X", b)
	}

	require.Equal(t, strings.Join(parts, ":"), got)

	_, err = web.Fingerprint("/nonexistent")
	require.Error(t, err)

	junk := filepath.Join(t.TempDir(), "junk.pem")
	require.NoError(t, os.WriteFile(junk, []byte("nope"), 0o600))

	_, err = web.Fingerprint(junk)
	require.Error(t, err)
}

package web

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"errors"
	"fmt"
	"math/big"
	"net"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const (
	certFileName = "web-cert.pem"
	keyFileName  = "web-key.pem"

	// macOS rejects server certificates valid for longer than 825 days.
	certLifetime = 800 * 24 * time.Hour
	// renewBefore is how long before expiry a new certificate is made. It
	// has to be trusted again on every device, so this is the warning period.
	renewBefore = 30 * 24 * time.Hour
	backdate    = time.Hour

	serialBits  = 128
	privateDir  = 0o700
	privateFile = 0o600
	publicFile  = 0o644
)

var errNoCertificate = errors.New("no certificate found")

// SelfSigned returns the certificate and key tamis serves HTTPS with, kept in
// dir. It makes a new pair if there is none, if it doesn't cover host, or if
// it expires within a month of now. Browsers accept passkeys only from a
// certificate they trust, so the certificate has to be installed on each
// device: the new pair needs trusting again.
func SelfSigned(dir, host string, now time.Time) (string, string, error) {
	certFile, keyFile := filepath.Join(dir, certFileName), filepath.Join(dir, keyFileName)

	if usable(certFile, keyFile, host, now) {
		return certFile, keyFile, nil
	}

	if err := os.MkdirAll(dir, privateDir); err != nil {
		return "", "", fmt.Errorf("create %s: %w", dir, err)
	}

	certPEM, keyPEM, err := newCertificate(host, now)
	if err != nil {
		return "", "", err
	}

	if err := os.WriteFile(keyFile, keyPEM, privateFile); err != nil {
		return "", "", fmt.Errorf("write %s: %w", keyFile, err)
	}

	if err := os.WriteFile(certFile, certPEM, publicFile); err != nil {
		return "", "", fmt.Errorf("write %s: %w", certFile, err)
	}

	return certFile, keyFile, nil
}

// usable reports whether the files hold a matching pair that covers host and
// won't expire soon.
func usable(certFile, keyFile, host string, now time.Time) bool {
	pair, err := tls.LoadX509KeyPair(certFile, keyFile)
	if err != nil {
		return false
	}

	cert, err := x509.ParseCertificate(pair.Certificate[0])
	if err != nil {
		return false
	}

	return cert.VerifyHostname(host) == nil &&
		now.After(cert.NotBefore) && now.Add(renewBefore).Before(cert.NotAfter)
}

func newCertificate(host string, now time.Time) ([]byte, []byte, error) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return nil, nil, fmt.Errorf("generate key: %w", err)
	}

	serial, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), serialBits))
	if err != nil {
		return nil, nil, fmt.Errorf("generate serial number: %w", err)
	}

	notBefore := now.Add(-backdate)
	template := &x509.Certificate{
		SerialNumber: serial,
		Subject:      pkix.Name{CommonName: host, Organization: []string{"tamis"}},
		NotBefore:    notBefore,
		NotAfter:     notBefore.Add(certLifetime),
		// A CA, so that trusting this one certificate is enough: it is its
		// own trust anchor.
		IsCA:                  true,
		BasicConstraintsValid: true,
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
	}

	if addr, err := netip.ParseAddr(host); err == nil {
		template.IPAddresses = []net.IP{addr.AsSlice()}
		template.DNSNames = []string{"localhost"}
	} else {
		template.DNSNames = []string{host, "localhost"}
	}

	der, err := x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key)
	if err != nil {
		return nil, nil, fmt.Errorf("create certificate: %w", err)
	}

	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, nil, fmt.Errorf("encode key: %w", err)
	}

	return pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}),
		pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: keyDER}), nil
}

// TLSListener makes listener speak HTTPS with the certificate in the files.
// The files are read once, so a renewed certificate needs a restart.
func TLSListener(listener net.Listener, certFile, keyFile string) (net.Listener, error) {
	pair, err := tls.LoadX509KeyPair(certFile, keyFile)
	if err != nil {
		return nil, fmt.Errorf("load certificate: %w", err)
	}

	return tls.NewListener(listener, &tls.Config{
		Certificates: []tls.Certificate{pair},
		MinVersion:   tls.VersionTLS12,
	}), nil
}

// Fingerprint is the SHA-256 fingerprint of the certificate in a PEM file, as
// colon-separated hex: what a browser or keychain shows to verify it.
func Fingerprint(certFile string) (string, error) {
	data, err := os.ReadFile(certFile) //nolint:gosec // the user's certificate
	if err != nil {
		return "", fmt.Errorf("read certificate: %w", err)
	}

	block, _ := pem.Decode(data)
	if block == nil || block.Type != "CERTIFICATE" {
		return "", fmt.Errorf("%s: %w", certFile, errNoCertificate)
	}

	sum := sha256.Sum256(block.Bytes)
	parts := make([]string, len(sum))

	for i, b := range sum {
		parts[i] = fmt.Sprintf("%02X", b)
	}

	return strings.Join(parts, ":"), nil
}

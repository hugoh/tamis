// Package secret keeps the account password in the operating system's
// credential store: the macOS Keychain, the Secret Service (GNOME Keyring,
// KWallet) on Linux, or the Windows Credential Manager.
package secret

import (
	"errors"
	"fmt"

	"github.com/zalando/go-keyring"
)

// ErrNotFound means no password is stored for the account.
var ErrNotFound = errors.New("no password stored")

// Keyring stores one secret per service and user.
type Keyring interface {
	Get(service, user string) (string, error)
	Set(service, user, password string) error
	Delete(service, user string) error
}

// Service names the keychain entry for an IMAP server, e.g.
// "tamis/imap.mail.me.com".
func Service(host string) string {
	return "tamis/" + host
}

// System is the operating system's credential store. On macOS it hands the
// password to /usr/bin/security over stdin, never on its command line.
type System struct{}

// Get implements Keyring.
func (System) Get(service, user string) (string, error) {
	password, err := keyring.Get(service, user)

	return password, translate(err)
}

// Set implements Keyring.
func (System) Set(service, user, password string) error {
	return translate(keyring.Set(service, user, password))
}

// Delete implements Keyring.
func (System) Delete(service, user string) error {
	return translate(keyring.Delete(service, user))
}

func translate(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, keyring.ErrNotFound):
		return ErrNotFound
	default:
		return fmt.Errorf("system keychain: %w", err)
	}
}

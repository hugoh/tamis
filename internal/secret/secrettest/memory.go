// Package secrettest provides a Keyring that keeps secrets in memory, for tests.
package secrettest

import (
	"sync"

	"github.com/hugoh/tamis/internal/secret"
)

// Memory is an in-process Keyring.
type Memory struct {
	mu      sync.Mutex
	secrets map[[2]string]string
}

// NewMemory returns an empty in-process Keyring.
func NewMemory() *Memory {
	return &Memory{secrets: map[[2]string]string{}}
}

// Get implements Keyring.
func (m *Memory) Get(service, user string) (string, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	password, ok := m.secrets[[2]string{service, user}]
	if !ok {
		return "", secret.ErrNotFound
	}

	return password, nil
}

// Set implements Keyring.
func (m *Memory) Set(service, user, password string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	m.secrets[[2]string{service, user}] = password

	return nil
}

// Delete implements Keyring.
func (m *Memory) Delete(service, user string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	key := [2]string{service, user}
	if _, ok := m.secrets[key]; !ok {
		return secret.ErrNotFound
	}

	delete(m.secrets, key)

	return nil
}

package store

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"
)

// Address is a whitelisted sender.
type Address struct {
	Address string
	Source  string
	AddedAt time.Time
}

// Addresses lists the whitelist, sorted by address.
func (s *Store) Addresses(ctx context.Context) ([]Address, error) {
	return s.readAddresses(ctx, "SELECT address, source, added_at FROM addresses ORDER BY address")
}

func (s *Store) readAddresses(ctx context.Context, query string, args ...any) ([]Address, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("list whitelist: %w", err)
	}

	defer func() { _ = rows.Close() }()

	var addresses []Address

	for rows.Next() {
		var (
			entry   Address
			addedAt int64
		)

		if err := rows.Scan(&entry.Address, &entry.Source, &addedAt); err != nil {
			return nil, fmt.Errorf("list whitelist: %w", err)
		}

		entry.AddedAt = time.Unix(addedAt, 0)
		addresses = append(addresses, entry)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list whitelist: %w", err)
	}

	return addresses, nil
}

// Passkey is a registered WebAuthn credential. Credential is opaque to the
// store: the web layer serializes it.
type Passkey struct {
	ID           int64
	CredentialID string
	Name         string
	Credential   []byte
	CreatedAt    time.Time
	LastUsed     time.Time
}

// AddPasskey registers a credential, or returns ErrExists if credentialID is
// already registered.
func (s *Store) AddPasskey(
	ctx context.Context,
	credentialID, name string,
	credential []byte,
) error {
	result, err := s.db.ExecContext(ctx,
		`INSERT INTO passkeys (credential_id, name, credential, created_at) VALUES (?, ?, ?, ?)
		 ON CONFLICT(credential_id) DO NOTHING`,
		credentialID, name, credential, time.Now().Unix())
	if err != nil {
		return fmt.Errorf("add passkey %s: %w", name, err)
	}

	if n, _ := result.RowsAffected(); n == 0 {
		return fmt.Errorf("passkey %s: %w", name, ErrExists)
	}

	return nil
}

// Passkeys lists the registered credentials, oldest first.
func (s *Store) Passkeys(ctx context.Context) ([]Passkey, error) {
	rows, err := s.db.QueryContext(
		ctx,
		"SELECT id, credential_id, name, credential, created_at, last_used FROM passkeys ORDER BY id",
	)
	if err != nil {
		return nil, fmt.Errorf("list passkeys: %w", err)
	}

	defer func() { _ = rows.Close() }()

	var keys []Passkey

	for rows.Next() {
		var (
			key              Passkey
			created, lastUse int64
		)

		if err := rows.Scan(
			&key.ID,
			&key.CredentialID,
			&key.Name,
			&key.Credential,
			&created,
			&lastUse,
		); err != nil {
			return nil, fmt.Errorf("list passkeys: %w", err)
		}

		key.CreatedAt = time.Unix(created, 0)
		if lastUse != 0 {
			key.LastUsed = time.Unix(lastUse, 0)
		}

		keys = append(keys, key)
	}

	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("list passkeys: %w", err)
	}

	return keys, nil
}

// UpdatePasskey stores the credential's new state after a login (its sign
// counter changes) and stamps the time of use.
func (s *Store) UpdatePasskey(ctx context.Context, credentialID string, credential []byte) error {
	result, err := s.db.ExecContext(ctx,
		"UPDATE passkeys SET credential = ?, last_used = ? WHERE credential_id = ?",
		credential, time.Now().Unix(), credentialID)
	if err != nil {
		return fmt.Errorf("update passkey: %w", err)
	}

	return foundOne(result, "passkey "+credentialID)
}

// RemovePasskey deletes the passkey with the given id.
func (s *Store) RemovePasskey(ctx context.Context, keyID int64) error {
	result, err := s.db.ExecContext(ctx, "DELETE FROM passkeys WHERE id = ?", keyID)
	if err != nil {
		return fmt.Errorf("remove passkey: %w", err)
	}

	return foundOne(result, fmt.Sprintf("passkey %d", keyID))
}

func foundOne(result sql.Result, what string) error {
	if n, _ := result.RowsAffected(); n == 0 {
		return fmt.Errorf("%s: %w", strings.TrimSpace(what), ErrNotFound)
	}

	return nil
}

// AddEnrollment stores the hash of a one-time enrollment token, and drops
// tokens that have already expired.
func (s *Store) AddEnrollment(ctx context.Context, tokenHash string, expires time.Time) error {
	txn, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("add enrollment: %w", err)
	}

	defer func() { _ = txn.Rollback() }()

	if _, err := txn.ExecContext(
		ctx,
		"DELETE FROM enrollments WHERE expires_at <= ?",
		time.Now().Unix(),
	); err != nil {
		return fmt.Errorf("add enrollment: %w", err)
	}

	if _, err := txn.ExecContext(ctx,
		"INSERT INTO enrollments (token_hash, expires_at) VALUES (?, ?)",
		tokenHash, expires.Unix()); err != nil {
		return fmt.Errorf("add enrollment: %w", err)
	}

	if err := txn.Commit(); err != nil {
		return fmt.Errorf("add enrollment: %w", err)
	}

	return nil
}

// ConsumeEnrollment uses up a token: it reports whether tokenHash was stored
// and unexpired at now. A token works once.
func (s *Store) ConsumeEnrollment(
	ctx context.Context,
	tokenHash string,
	now time.Time,
) (bool, error) {
	result, err := s.db.ExecContext(ctx,
		"DELETE FROM enrollments WHERE token_hash = ? AND expires_at > ?", tokenHash, now.Unix())
	if err != nil {
		return false, fmt.Errorf("use enrollment token: %w", err)
	}

	n, _ := result.RowsAffected()

	return n > 0, nil
}

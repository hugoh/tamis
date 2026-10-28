package secret_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/secret"
	"github.com/stretchr/testify/require"
	"github.com/zalando/go-keyring"
)

func exercise(t *testing.T, ring secret.Keyring) {
	t.Helper()

	const service, user = "tamis/imap.example.com", "me@example.com"

	_, err := ring.Get(service, user)
	require.ErrorIs(t, err, secret.ErrNotFound)

	require.NoError(t, ring.Set(service, user, "s3cret"))

	got, err := ring.Get(service, user)
	require.NoError(t, err)
	require.Equal(t, "s3cret", got)

	require.NoError(t, ring.Delete(service, user))
	require.ErrorIs(t, ring.Delete(service, user), secret.ErrNotFound)
}

func TestMemory(t *testing.T) {
	t.Parallel()

	exercise(t, secret.NewMemory())
}

// TestSystem runs against go-keyring's in-process mock, never the real
// keychain. It replaces a package-global, so it can't run in parallel.
func TestSystem(t *testing.T) { //nolint:paralleltest // see above
	keyring.MockInit()

	exercise(t, secret.System{})
}

func TestService(t *testing.T) {
	t.Parallel()

	require.Equal(t, "tamis/imap.mail.me.com", secret.Service("imap.mail.me.com"))
}

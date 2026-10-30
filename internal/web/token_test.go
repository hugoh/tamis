package web_test

import (
	"testing"

	"github.com/hugoh/tamis/internal/web"
	"github.com/stretchr/testify/require"
)

func TestNewToken(t *testing.T) {
	t.Parallel()

	first, err := web.NewToken()
	require.NoError(t, err)

	second, err := web.NewToken()
	require.NoError(t, err)

	require.NotEqual(t, first, second)
	require.GreaterOrEqual(t, len(first), 43, "256 bits in base64url")
	require.NotContains(t, first, "=")
}

func TestHashToken(t *testing.T) {
	t.Parallel()

	hash := web.HashToken("abc")

	require.Equal(t, hash, web.HashToken("abc"))
	require.NotEqual(t, hash, web.HashToken("abx"))
	require.NotContains(t, hash, "abc")
	require.Len(t, hash, 64)
}

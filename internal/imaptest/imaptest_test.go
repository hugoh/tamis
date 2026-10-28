package imaptest_test

import (
	"testing"

	"github.com/emersion/go-imap/v2/imapclient"
	"github.com/hugoh/tamis/internal/imaptest"
	"github.com/stretchr/testify/require"
)

func TestStartWithTLS(t *testing.T) {
	t.Parallel()

	srv := imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true})

	client, err := imapclient.DialTLS(srv.Addr, &imapclient.Options{TLSConfig: srv.TLSConfig})
	require.NoError(t, err)

	defer func() { _ = client.Close() }()

	require.NoError(t, client.Login(imaptest.Username, imaptest.Password).Wait())

	if _, err := client.Select("Junk", nil).Wait(); err != nil {
		t.Fatal(err)
	}

	require.NoError(t, client.Logout().Wait())
}

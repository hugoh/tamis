package imaptest_test

import (
	"testing"

	"github.com/emersion/go-imap/v2/imapclient"
	"github.com/hugoh/tamis/internal/imaptest"
)

func TestStartWithTLS(t *testing.T) {
	t.Parallel()

	srv := imaptest.StartWith(t, imaptest.Options{Caps: imaptest.Rev2(), TLS: true})

	client, err := imapclient.DialTLS(srv.Addr, &imapclient.Options{TLSConfig: srv.TLSConfig})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = client.Close() }()

	if err := client.Login(imaptest.Username, imaptest.Password).Wait(); err != nil {
		t.Fatal(err)
	}

	if _, err := client.Select("Junk", nil).Wait(); err != nil {
		t.Fatal(err)
	}

	if err := client.Logout().Wait(); err != nil {
		t.Fatal(err)
	}
}

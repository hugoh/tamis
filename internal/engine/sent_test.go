package engine_test

import (
	"bytes"
	"fmt"
	netmail "net/mail"
	"testing"

	"github.com/hugoh/tamis/internal/config"
	"github.com/stretchr/testify/require"
)

const (
	myAddress = "me@example.com"
	colleague = "colleague@example.org"
)

func TestSentRecipientsAreWhitelisted(t *testing.T) {
	t.Parallel()

	e := setup(t, config.ModeShadow)
	e.cfg.Account.User = myAddress
	e.append(t, sent, []byte("From: "+myAddress+"\r\nTo: before@example.com\r\n\r\nhi\r\n"))
	e.run(t)

	e.append(t, sent, fmt.Appendf(nil,
		"From: "+myAddress+"\r\nTo: Friend <Friend@Example.com>, me@example.com\r\n"+
			"Cc: colleague@example.org\r\nSubject: hi\r\n\r\nhello\r\n"))
	e.run(t)

	for address, want := range map[string]bool{
		friend:               true,
		colleague:            true,
		myAddress:            false,
		"before@example.com": false,
	} {
		ok, err := e.db.HasAddress(t.Context(), address)
		require.NoError(t, err)
		require.Equal(t, want, ok, address)
	}
}

func TestSentReplyTeachesGood(t *testing.T) {
	t.Parallel()

	const stranger = "u@unknown.example"

	tests := []struct {
		name        string
		mode        config.Mode
		original    func(e *env) []byte
		unseen      bool
		whitelisted bool
	}{
		{
			name: "reply to uncertain mail",
			mode: config.ModeShadow,
			original: func(e *env) []byte {
				return e.message(stranger, "Quokka", "Xylophone zebra quokka.")
			},
			whitelisted: true,
		},
		{
			name:        "reply to mail filed as spam",
			mode:        config.ModeLive,
			original:    func(e *env) []byte { return e.spammy(stranger) },
			whitelisted: true,
		},
		{
			name: "reply to mail tamis found good",
			mode: config.ModeShadow,
			original: func(e *env) []byte {
				return e.hammy(stranger)
			},
		},
		{
			name: "reply to mail tamis never saw",
			mode: config.ModeShadow,
			original: func(e *env) []byte {
				return e.message(stranger, "Quokka", "Xylophone zebra quokka.")
			},
			unseen: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			e := setup(t, test.mode)
			e.trainModel(t)
			require.NoError(t, e.client.Ensure(sent))
			e.run(t)
			original := test.original(e)
			parsed, err := netmail.ReadMessage(bytes.NewReader(original))
			require.NoError(t, err)

			inReplyTo := parsed.Header.Get("Message-ID")
			if test.unseen {
				inReplyTo = "<other@test>"
			}

			e.append(t, inbox, original)
			e.run(t)

			e.append(t, sent, fmt.Appendf(
				nil,
				"From: %s\r\nTo: nobody@example.net\r\nIn-Reply-To: %s\r\nSubject: Re\r\n\r\nthanks\r\n",
				myAddress,
				inReplyTo,
			))
			e.run(t)

			ok, err := e.db.HasAddress(t.Context(), stranger)
			require.NoError(t, err)
			require.Equal(t, test.whitelisted, ok)
		})
	}
}

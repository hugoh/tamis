package web_test

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func get(t *testing.T, url string) (*http.Response, error) {
	t.Helper()

	req, err := http.NewRequestWithContext(t.Context(), http.MethodGet, url, nil)
	require.NoError(t, err)

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("get %s: %w", url, err)
	}

	return resp, nil
}

func TestServeAndShutDown(t *testing.T) {
	t.Parallel()

	var lc net.ListenConfig

	listener, err := lc.Listen(t.Context(), "tcp", "127.0.0.1:0")
	require.NoError(t, err)

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	done := make(chan error, 1)
	srv := newHarness(t).srv

	go func() { done <- srv.Serve(ctx, listener) }()

	url := "http://" + listener.Addr().String() + loginPath

	resp, err := get(t, url)
	require.NoError(t, err)
	require.NoError(t, resp.Body.Close())
	require.Equal(t, http.StatusOK, resp.StatusCode)

	cancel()

	select {
	case err := <-done:
		require.NoError(t, err, "a requested shutdown is not an error")
	case <-time.After(5 * time.Second):
		t.Fatal("server did not shut down")
	}

	if resp, err := get(t, url); err == nil {
		_ = resp.Body.Close()

		t.Fatal("server still answers after shutdown")
	}
}

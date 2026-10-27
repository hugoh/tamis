package probe

import (
	"bufio"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"strings"
	"time"
)

// errRawCommand is returned when the server rejects a raw command.
var errRawCommand = errors.New("command rejected")

// rawEnable logs in on a fresh connection and sends ENABLE with the given
// capabilities, returning the ones the server confirmed in its ENABLED
// response.
//
// imapclient refuses to ENABLE extensions it cannot parse (CONDSTORE and
// QRESYNC among them), and iCloud has been reported to advertise QRESYNC
// without honouring it, so this speaks just enough raw IMAP to find out.
func rawEnable(ctx context.Context, cfg Config, caps ...string) ([]string, error) {
	conn, err := rawDial(ctx, cfg)
	if err != nil {
		return nil, err
	}

	defer func() { _ = conn.Close() }()

	reader := bufio.NewReader(conn)

	if _, err := readUntil(reader, "*"); err != nil {
		return nil, fmt.Errorf("greeting: %w", err)
	}

	login := fmt.Sprintf("LOGIN %s %s", quote(cfg.Username), quote(cfg.Password))
	if _, err := rawCommand(conn, reader, "a1", login); err != nil {
		return nil, fmt.Errorf("login: %w", err)
	}

	lines, err := rawCommand(conn, reader, "a2", "ENABLE "+strings.Join(caps, " "))
	if err != nil {
		return nil, fmt.Errorf("enable: %w", err)
	}

	_, _ = rawCommand(conn, reader, "a3", "LOGOUT")

	var enabled []string

	for _, line := range lines {
		if rest, ok := strings.CutPrefix(line, "* ENABLED"); ok {
			enabled = append(enabled, strings.Fields(strings.ToUpper(rest))...)
		}
	}

	return enabled, nil
}

func rawDial(ctx context.Context, cfg Config) (net.Conn, error) {
	timeout := cfg.Timeout
	if timeout == 0 {
		timeout = defaultTimeout
	}

	dialer := &net.Dialer{Timeout: timeout}

	var (
		conn net.Conn
		err  error
	)

	if cfg.Plaintext {
		conn, err = dialer.DialContext(ctx, "tcp", cfg.Addr)
	} else {
		tlsDialer := &tls.Dialer{NetDialer: dialer, Config: cfg.TLSConfig}
		conn, err = tlsDialer.DialContext(ctx, "tcp", cfg.Addr)
	}

	if err != nil {
		return nil, fmt.Errorf("connect to %s: %w", cfg.Addr, err)
	}

	_ = conn.SetDeadline(time.Now().Add(timeout))

	return conn, nil
}

// rawCommand sends one tagged command and returns the untagged lines that
// preceded its completion.
func rawCommand(conn net.Conn, r *bufio.Reader, tag, command string) ([]string, error) {
	if _, err := fmt.Fprintf(conn, "%s %s\r\n", tag, command); err != nil {
		return nil, fmt.Errorf("write: %w", err)
	}

	lines, err := readUntil(r, tag+" ")
	if err != nil {
		return lines, err
	}

	status := lines[len(lines)-1]
	if !strings.HasPrefix(status, tag+" OK") {
		return lines, fmt.Errorf("%w: %s", errRawCommand, status)
	}

	return lines[:len(lines)-1], nil
}

// readUntil reads lines until one starts with prefix, returning all of them.
func readUntil(r *bufio.Reader, prefix string) ([]string, error) {
	var lines []string

	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return lines, fmt.Errorf("read: %w", err)
		}

		line = strings.TrimRight(line, "\r\n")
		lines = append(lines, line)

		if strings.HasPrefix(line, prefix) {
			return lines, nil
		}
	}
}

// quote renders s as an IMAP quoted string.
func quote(s string) string {
	return `"` + strings.NewReplacer(`\`, `\\`, `"`, `\"`).Replace(s) + `"`
}

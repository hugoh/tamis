package cli

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/secret"
)

const (
	passwordEnv = "TAMIS_PASSWORD"
	// credentialName is the systemd credential tamis reads, e.g. from
	// LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis.
	credentialName = "password"
)

var (
	errNoPassword = errors.New("no password: run 'tamis login', or set account.password_command " +
		"or $" + passwordEnv)
	errNoKeychain = errors.New("no system keychain available")
)

// credentials identifies whose password to find.
type credentials struct {
	host, user string
	command    []string
}

// passwordFinder looks for the password in, by order of preference:
//
//  1. account.password_command, when configured;
//  2. the systemd credential $CREDENTIALS_DIRECTORY/password;
//  3. the system keychain (see 'tamis login');
//  4. $TAMIS_PASSWORD, which other processes can read: a last resort;
//  5. typing it, on a terminal, or on stdin when stdin is set.
//
// A typed password is kept, so a long-running command asks only once.
type passwordFinder struct {
	env   Env
	stdin io.Reader
	typed string
	// command is what password_command printed, so a long-running command
	// doesn't call the vault on every pass.
	command string
}

func (f *passwordFinder) findFromCommand(ctx context.Context, command []string) (string, error) {
	if f.command != "" {
		return f.command, nil
	}

	password, err := config.Account{PasswordCommand: command}.Password(ctx)
	if err != nil {
		return "", fmt.Errorf("password: %w", err)
	}

	f.command = password

	return password, nil
}

func (f *passwordFinder) find(ctx context.Context, who credentials) (string, error) {
	if len(who.command) > 0 {
		return f.findFromCommand(ctx, who.command)
	}

	if password, found, err := systemdCredential(f.env); found || err != nil {
		return password, err
	}

	if f.env.Keyring != nil {
		// An unreachable keychain (a headless server) falls through.
		if password, err := f.env.Keyring.Get(secret.Service(who.host), who.user); err == nil {
			return password, nil
		}
	}

	if password := f.env.Getenv(passwordEnv); password != "" {
		return password, nil
	}

	if f.typed != "" {
		return f.typed, nil
	}

	password, err := typePassword(f.env, f.stdin, who.user)
	if err != nil {
		return "", err
	}

	f.typed = password

	return password, nil
}

// systemdCredential reads the password systemd passes a service through
// LoadCredential= or LoadCredentialEncrypted=.
func systemdCredential(env Env) (string, bool, error) {
	dir := env.Getenv("CREDENTIALS_DIRECTORY")
	if dir == "" {
		return "", false, nil
	}

	path := filepath.Join(dir, credentialName)

	content, err := os.ReadFile(path) //nolint:gosec // systemd chose the directory
	if errors.Is(err, fs.ErrNotExist) {
		return "", false, nil
	}

	if err != nil {
		return "", true, fmt.Errorf("read systemd credential: %w", err)
	}

	return strings.TrimRight(string(content), "\r\n"), true, nil
}

// typePassword asks on the terminal, or reads the first line of stdin
// (when given) if there is no terminal.
func typePassword(env Env, stdin io.Reader, user string) (string, error) {
	if env.Prompt != nil {
		password, err := env.Prompt(fmt.Sprintf("Password for %s: ", user))

		switch {
		case errors.Is(err, ErrNotTerminal):
		case err != nil:
			return "", fmt.Errorf("read password: %w", err)
		case password == "":
			return "", errNoPassword
		default:
			return password, nil
		}
	}

	if stdin == nil {
		return "", errNoPassword
	}

	line, err := bufio.NewReader(stdin).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("read password: %w", err)
	}

	password := strings.TrimRight(line, "\r\n")
	if password == "" {
		return "", errNoPassword
	}

	return password, nil
}

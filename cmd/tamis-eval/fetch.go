package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/hugoh/tamis/internal/config"
	"github.com/hugoh/tamis/internal/mail"
	"github.com/hugoh/tamis/internal/secret"
)

const (
	fetchBatch  = 50
	dirMode     = 0o700
	fileMode    = 0o600
	passwordEnv = "TAMIS_PASSWORD"
)

var errSpec = errors.New("want Folder=N, with N above zero")

// specs collects repeated --spam or --ham flags.
type specs []string

func (s *specs) String() string { return strings.Join(*s, ",") }

func (s *specs) Set(value string) error {
	*s = append(*s, value)

	return nil
}

// parseSpec reads "Folder=N": a folder and how many of its latest messages.
func parseSpec(spec string) (string, int, error) {
	cut := strings.LastIndex(spec, "=")
	if cut <= 0 {
		return "", 0, fmt.Errorf("%q: %w", spec, errSpec)
	}

	limit, err := strconv.Atoi(spec[cut+1:])
	if err != nil || limit <= 0 {
		return "", 0, fmt.Errorf("%q: %w", spec, errSpec)
	}

	return spec[:cut], limit, nil
}

func fetchCommand(args []string, out, errOut io.Writer) error {
	flags := flag.NewFlagSet("fetch", flag.ContinueOnError)
	flags.SetOutput(errOut)

	var spam, ham specs

	path := flags.String("config", config.DefaultPath(os.Getenv), "tamis configuration file")
	flags.Var(&spam, "spam", "Folder=N: the latest N messages of Folder are spam (repeatable)")
	flags.Var(&ham, "ham", "Folder=N: the latest N messages of Folder are good (repeatable)")

	if err := flags.Parse(args); err != nil {
		return fmt.Errorf("fetch: %w", err)
	}

	if flags.NArg() != 1 || len(spam) == 0 || len(ham) == 0 {
		return fmt.Errorf("%w: fetch needs --spam, --ham and a folder to write to", errUsage)
	}

	root, err := filepath.Abs(flags.Arg(0))
	if err != nil {
		return fmt.Errorf("fetch: %w", err)
	}

	if err := checkOutsideRepo(root); err != nil {
		return err
	}

	client, err := connect(context.Background(), *path)
	if err != nil {
		return err
	}

	defer func() { _ = client.Close() }()

	for class, folders := range map[string]specs{"spam": spam, "ham": ham} {
		if err := fetchClass(client, filepath.Join(root, class), folders, out); err != nil {
			return err
		}
	}

	_, _ = fmt.Fprintf(out, "\nThat is a copy of your mail. Delete %s when you are done.\n", root)

	return nil
}

// fetchClass fetches each Folder=N spec into its own folder below dir.
func fetchClass(client folderSource, dir string, folders specs, out io.Writer) error {
	for _, spec := range folders {
		folder, limit, err := parseSpec(spec)
		if err != nil {
			return err
		}

		target := filepath.Join(dir, strings.NewReplacer("/", "_", " ", "_").Replace(folder))

		written, err := fetchFolder(client, folder, limit, target)
		if err != nil {
			return err
		}

		_, _ = fmt.Fprintf(out, "%s: wrote %d messages to %s\n", folder, written, target)
	}

	return nil
}

func connect(ctx context.Context, path string) (*mail.Client, error) {
	cfg, err := config.Load(os.Getenv, path)
	if err != nil {
		return nil, fmt.Errorf("fetch: %w", err)
	}

	password := os.Getenv(passwordEnv)
	if password == "" {
		password, err = secret.System{}.Get(secret.Service(cfg.Account.Host), cfg.Account.User)
		if err != nil {
			return nil, fmt.Errorf("fetch: %w (run tamis login, or set $%s)", err, passwordEnv)
		}
	}

	client, err := mail.Dial(ctx, mail.DialConfig{
		Addr: cfg.Account.Addr(), Username: cfg.Account.User, Password: password,
	})
	if err != nil {
		return nil, fmt.Errorf("fetch: %w", err)
	}

	return client, nil
}

// folderSource is the part of the IMAP client fetchFolder uses.
type folderSource interface {
	UIDsAfter(mailbox string, after uint32) ([]uint32, error)
	Fetch(mailbox string, uids []uint32) ([]mail.Message, error)
}

// fetchFolder writes the latest limit messages of folder to dir, one file
// each. Fetching peeks, so nothing is marked as read.
func fetchFolder(client folderSource, folder string, limit int, dir string) (int, error) {
	uids, err := client.UIDsAfter(folder, 0)
	if err != nil {
		return 0, fmt.Errorf("list %s: %w", folder, err)
	}

	uids = uids[max(0, len(uids)-limit):]

	if err := os.MkdirAll(dir, dirMode); err != nil {
		return 0, fmt.Errorf("create %s: %w", dir, err)
	}

	written := 0

	for batch := range slices.Chunk(uids, fetchBatch) {
		messages, err := client.Fetch(folder, batch)
		if err != nil {
			return written, fmt.Errorf("fetch from %s: %w", folder, err)
		}

		for _, message := range messages {
			name := filepath.Join(dir, fmt.Sprintf("%08d.eml", message.UID))
			if err := os.WriteFile(name, message.Raw, fileMode); err != nil {
				return written, fmt.Errorf("write %s: %w", name, err)
			}

			written++
		}
	}

	return written, nil
}

// checkOutsideRepo refuses a directory inside a git or jj working tree: jj
// snapshots every file it finds, so mail written there would end up in a
// commit.
func checkOutsideRepo(path string) error {
	for dir := path; ; dir = filepath.Dir(dir) {
		for _, marker := range []string{".git", ".jj"} {
			if _, err := os.Stat(filepath.Join(dir, marker)); err == nil {
				return fmt.Errorf(
					"%w: %s is inside the repository at %s; use a directory outside it",
					errInRepo,
					path,
					dir,
				)
			}
		}

		if dir == filepath.Dir(dir) {
			return nil
		}
	}
}

var errInRepo = errors.New("refusing to copy mail into a repository")

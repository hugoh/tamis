// Package config loads tamis's TOML configuration.
package config

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"github.com/pelletier/go-toml/v2"
)

// Mode says whether tamis acts on its decisions.
type Mode string

// Modes.
const (
	// ModeShadow records what tamis would do and changes nothing.
	ModeShadow Mode = "shadow"
	// ModeLive files messages according to their verdict.
	ModeLive Mode = "live"
)

var errInvalid = errors.New("invalid config")

// ErrNoPasswordCommand means account.password_command isn't set.
var ErrNoPasswordCommand = errors.New("account.password_command is not set")

// Config is the whole configuration file.
type Config struct {
	// State is the SQLite database path; "~/" expands to the home directory.
	State   string  `toml:"state"`
	Account Account `toml:"account"`
	Folders Folders `toml:"folders"`
	Policy  Policy  `toml:"policy"`
}

// Account is how to reach the mailbox.
type Account struct {
	Host string `toml:"host"`
	Port int    `toml:"port"`
	User string `toml:"user"`
	// PasswordCommand is run without a shell; its trimmed stdout is the
	// password. For example: ["security", "find-generic-password", "-s",
	// "tamis", "-w"].
	PasswordCommand []string `toml:"password_command"`
	// AuthServerID is the authserv-id of the provider's Authentication-Results
	// header, for example "mx.google.com". When set, other headers are ignored.
	AuthServerID string `toml:"auth_server_id"`
}

// Folders names the mailboxes tamis reads and writes.
type Folders struct {
	Watch     []string `toml:"watch"`
	Junk      string   `toml:"junk"`
	Sent      string   `toml:"sent"`
	TrainSpam string   `toml:"train_spam"`
	TrainGood string   `toml:"train_good"`
	// Rescued is where mail rescued from Junk goes; empty means the first
	// watched folder.
	Rescued string `toml:"rescued"`
}

// RescueTo is the folder rescued mail is moved to.
func (f Folders) RescueTo() string {
	if f.Rescued != "" {
		return f.Rescued
	}

	return f.Watch[0]
}

// Policy turns scores into actions.
type Policy struct {
	Mode Mode `toml:"mode"`
	// Scores at or above SpamThreshold are spam; at or below GoodThreshold,
	// good; anything between is uncertain.
	SpamThreshold float64  `toml:"spam_threshold"`
	GoodThreshold float64  `toml:"good_threshold"`
	PollInterval  Duration `toml:"poll_interval"`
	// WhitelistRequiresAuth only trusts a whitelisted sender when DKIM or
	// DMARC passed, so a forged From can't bypass the filter.
	WhitelistRequiresAuth bool `toml:"whitelist_requires_auth"`
	// LearnFromJunk treats a message tamis judged good or uncertain that
	// later appears in Junk as one the user moved there: missed spam. Turn
	// it off if another program (e.g. Mail.app's junk filter) moves mail.
	LearnFromJunk bool `toml:"learn_from_junk"`
	// LearnFromKeywords learns from the Junk / Not Junk marks mail clients
	// set ($Junk, $NotJunk, and Apple Mail's Junk, NotJunk) when they
	// contradict tamis's own verdict.
	LearnFromKeywords bool `toml:"learn_from_keywords"`
	// Rescue moves mail the provider filed to Junk back out when its sender
	// is whitelisted and DKIM or DMARC passed (regardless of
	// WhitelistRequiresAuth: impersonated contacts are what Junk catches).
	Rescue bool `toml:"rescue"`
	// RescueThreshold also rescues mail scoring at or below it. Zero turns
	// score-based rescue off.
	RescueThreshold float64 `toml:"rescue_threshold"`
}

// Duration is a time.Duration written as a string such as "2m".
type Duration struct {
	time.Duration
}

// UnmarshalText implements encoding.TextUnmarshaler.
func (d *Duration) UnmarshalText(text []byte) error {
	parsed, err := time.ParseDuration(string(text))
	if err != nil {
		return fmt.Errorf("duration %q: %w", text, err)
	}

	d.Duration = parsed

	return nil
}

func defaults() Config {
	return Config{
		Account: Account{
			Port: 993, //nolint:mnd // IMAPS
		},
		Folders: Folders{
			Watch:     []string{"INBOX"},
			Junk:      "Junk",
			Sent:      "Sent Messages",
			TrainSpam: "TrainSpam",
			TrainGood: "TrainGood",
		},
		Policy: Policy{
			Mode:                  ModeShadow,
			SpamThreshold:         0.9, //nolint:mnd // SpamBayes' spam cutoff
			GoodThreshold:         0.2, //nolint:mnd // SpamBayes' ham cutoff
			PollInterval:          Duration{2 * time.Minute},
			WhitelistRequiresAuth: true,
			LearnFromJunk:         true,
			LearnFromKeywords:     true,
			Rescue:                true,
		},
	}
}

// DefaultPath is $XDG_CONFIG_HOME/tamis/config.toml, falling back to
// ~/.config. It is empty when there is no home directory either.
func DefaultPath(getenv func(string) string) string {
	base := getenv("XDG_CONFIG_HOME")
	if base == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}

		base = filepath.Join(home, ".config")
	}

	return filepath.Join(base, "tamis", "config.toml")
}

// Load reads and validates the file at path, filling unset fields with
// defaults. Unknown keys are errors, so typos don't pass silently.
func Load(getenv func(string) string, path string) (*Config, error) {
	if path == "" {
		return nil, errNoHome
	}

	file, err := os.Open(path) //nolint:gosec // the path is the user's own config file
	if err != nil {
		return nil, fmt.Errorf("read config: %w", err)
	}

	defer func() { _ = file.Close() }()

	if err := checkNotWritableByOthers(file); err != nil {
		return nil, err
	}

	cfg := defaults()

	if err := toml.NewDecoder(file).DisallowUnknownFields().Decode(&cfg); err != nil {
		return nil, decodeError(path, err)
	}

	if err := cfg.validate(); err != nil {
		return nil, err
	}

	if cfg.State == "" {
		cfg.State = DefaultStatePath(getenv)
	}

	if cfg.State == "" {
		return nil, fmt.Errorf("state: %w", errNoHome)
	}

	cfg.State, err = expandHome(cfg.State)
	if err != nil {
		return nil, err
	}

	return &cfg, nil
}

// DefaultStatePath is $XDG_STATE_HOME/tamis/tamis.db, falling back to
// ~/.local/state. It is empty when there is no home directory either.
func DefaultStatePath(getenv func(string) string) string {
	base := getenv("XDG_STATE_HOME")
	if base == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}

		base = filepath.Join(home, ".local", "state")
	}

	return filepath.Join(base, "tamis", "tamis.db")
}

// checkNotWritableByOthers refuses a config file other users could edit:
// password_command runs whatever it names.
func checkNotWritableByOthers(file *os.File) error {
	if runtime.GOOS == "windows" {
		return nil
	}

	info, err := file.Stat()
	if err != nil {
		return fmt.Errorf("read config: %w", err)
	}

	if info.Mode().Perm()&0o022 != 0 {
		return fmt.Errorf("%w: %s is writable by others, and password_command runs what it names; "+
			"fix with: chmod go-w %s", errInvalid, file.Name(), file.Name())
	}

	return nil
}

func decodeError(path string, err error) error {
	if strict, ok := errors.AsType[*toml.StrictMissingError](err); ok {
		keys := make([]string, len(strict.Errors))
		for i, missing := range strict.Errors {
			keys[i] = strings.Join(missing.Key(), ".")
		}

		return fmt.Errorf("%w: unknown keys %s", errInvalid, strings.Join(keys, ", "))
	}

	if decode, ok := errors.AsType[*toml.DecodeError](err); ok && len(decode.Key()) > 0 {
		return fmt.Errorf("parse %s: %s: %w", path, strings.Join(decode.Key(), "."), err)
	}

	return fmt.Errorf("parse %s: %w", path, err)
}

func (c *Config) validate() error {
	var problems []string

	if c.Account.Host == "" {
		problems = append(problems, "account.host is required, e.g. imap.mail.me.com for iCloud")
	}

	if c.Account.User == "" {
		problems = append(problems, "account.user is required")
	}

	if len(c.Folders.Watch) == 0 {
		problems = append(problems, "folders.watch needs at least one folder")
	}

	problems = append(problems, c.Policy.problems()...)

	if len(problems) > 0 {
		return fmt.Errorf("%w: %s", errInvalid, strings.Join(problems, "; "))
	}

	return nil
}

func (p Policy) problems() []string {
	var problems []string

	if p.Mode != ModeShadow && p.Mode != ModeLive {
		problems = append(
			problems,
			fmt.Sprintf("policy.mode must be %q or %q", ModeShadow, ModeLive),
		)
	}

	if !(0 < p.GoodThreshold && p.GoodThreshold < p.SpamThreshold && p.SpamThreshold < 1) {
		problems = append(problems, "need 0 < policy.good_threshold < policy.spam_threshold < 1")
	}

	if p.RescueThreshold < 0 || p.RescueThreshold >= p.GoodThreshold {
		problems = append(problems, "need 0 <= policy.rescue_threshold < policy.good_threshold")
	}

	return problems
}

// Addr is the server's host:port.
func (a Account) Addr() string {
	return net.JoinHostPort(a.Host, strconv.Itoa(a.Port))
}

// Password runs PasswordCommand. It returns ErrNoPasswordCommand when none
// is configured, so callers can try other sources.
func (a Account) Password(ctx context.Context) (string, error) {
	if len(a.PasswordCommand) == 0 {
		return "", ErrNoPasswordCommand
	}

	//nolint:gosec // the command comes from the user's own config file
	out, err := exec.CommandContext(ctx, a.PasswordCommand[0], a.PasswordCommand[1:]...).Output()
	if err != nil {
		return "", fmt.Errorf("account.password_command: %w", err)
	}

	password := strings.TrimRight(string(out), "\r\n")
	if password == "" {
		return "", fmt.Errorf("account.password_command: %w", errEmptyPassword)
	}

	return password, nil
}

var errNoHome = errors.New(
	"no home directory: set XDG_CONFIG_HOME and XDG_STATE_HOME, pass --config, or set state in the config file",
)

var errEmptyPassword = errors.New("printed an empty password")

func expandHome(path string) (string, error) {
	rest, ok := strings.CutPrefix(path, "~/")
	if !ok {
		return path, nil
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("expand %s: %w", path, err)
	}

	return filepath.Join(home, rest), nil
}

package main

import (
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/hugoh/tamis/internal/config"
)

type configCase struct {
	File   string         `json:"file"`
	Error  string         `json:"error,omitempty"`
	Config *configSummary `json:"config,omitempty"`
}

type configSummary struct {
	State                 string   `json:"state"`
	Host                  string   `json:"host"`
	Port                  int      `json:"port"`
	Addr                  string   `json:"addr"`
	User                  string   `json:"user"`
	PasswordCommand       []string `json:"password_command"`
	AuthServerID          string   `json:"auth_server_id"`
	Watch                 []string `json:"watch"`
	Junk                  string   `json:"junk"`
	Sent                  string   `json:"sent"`
	TrainSpam             string   `json:"train_spam"`
	TrainGood             string   `json:"train_good"`
	RescueTo              string   `json:"rescue_to"`
	Mode                  string   `json:"mode"`
	SpamThreshold         float64  `json:"spam_threshold"`
	GoodThreshold         float64  `json:"good_threshold"`
	PollIntervalNanos     int64    `json:"poll_interval_nanos"`
	WhitelistRequiresAuth bool     `json:"whitelist_requires_auth"`
	LearnFromJunk         bool     `json:"learn_from_junk"`
	LearnProviderJunk     bool     `json:"learn_provider_junk"`
	LearnFromKeywords     bool     `json:"learn_from_keywords"`
	Rescue                bool     `json:"rescue"`
	JournalRetentionNanos int64    `json:"journal_retention_nanos"`
	RescueThreshold       float64  `json:"rescue_threshold"`
	WebEnabled            bool     `json:"web_enabled"`
	WebListen             string   `json:"web_listen"`
	WebOrigin             string   `json:"web_origin"`
	WebTLSCert            string   `json:"web_tls_cert"`
	WebTLSKey             string   `json:"web_tls_key"`
	WebSelfSigned         bool     `json:"web_self_signed"`
	WebTLS                bool     `json:"web_tls"`
	WebRPID               string   `json:"web_rpid"`
	MonitorPingURL        string   `json:"monitor_ping_url"`
	MonitorQualityURL     string   `json:"monitor_quality_url"`
	MonitorFailAfter      int      `json:"monitor_fail_after"`
	RulesScript           string   `json:"rules_script"`
	RulesCreateFolders    bool     `json:"rules_create_folders"`
}

func summarize(cfg *config.Config) *configSummary {
	return &configSummary{
		State: cfg.State, Host: cfg.Account.Host, Port: cfg.Account.Port, Addr: cfg.Account.Addr(),
		User: cfg.Account.User, PasswordCommand: cfg.Account.PasswordCommand,
		AuthServerID: cfg.Account.AuthServerID, Watch: cfg.Folders.Watch, Junk: cfg.Folders.Junk,
		Sent: cfg.Folders.Sent, TrainSpam: cfg.Folders.TrainSpam, TrainGood: cfg.Folders.TrainGood,
		RescueTo: cfg.Folders.RescueTo(), Mode: string(cfg.Policy.Mode),
		SpamThreshold: cfg.Policy.SpamThreshold, GoodThreshold: cfg.Policy.GoodThreshold,
		PollIntervalNanos:     int64(cfg.Policy.PollInterval.Duration / time.Nanosecond),
		WhitelistRequiresAuth: cfg.Policy.WhitelistRequiresAuth, LearnFromJunk: cfg.Policy.LearnFromJunk,
		LearnProviderJunk: cfg.Policy.LearnProviderJunk, LearnFromKeywords: cfg.Policy.LearnFromKeywords,
		Rescue:                cfg.Policy.Rescue,
		JournalRetentionNanos: int64(cfg.Policy.JournalRetention.Duration / time.Nanosecond),
		RescueThreshold:       cfg.Policy.RescueThreshold, WebEnabled: cfg.Web.Enabled,
		WebListen: cfg.Web.Listen, WebOrigin: cfg.Web.Origin, WebTLSCert: cfg.Web.TLSCert,
		WebTLSKey: cfg.Web.TLSKey, WebSelfSigned: cfg.Web.SelfSigned, WebTLS: cfg.Web.TLS(),
		WebRPID: cfg.Web.RPID(), MonitorPingURL: cfg.Monitor.PingURL,
		MonitorQualityURL: cfg.Monitor.QualityURL, MonitorFailAfter: cfg.Monitor.FailAfter,
		RulesScript: cfg.Rules.Script, RulesCreateFolders: cfg.Rules.CreateFolders,
	}
}

// configCases loads every config fixture. Errors from the TOML parser itself
// are recorded as "parse" only, since the Rust parser words them differently.
func configCases(dir string) ([]configCase, error) {
	files, err := filepath.Glob(filepath.Join(dir, "*.toml"))
	if err != nil {
		return nil, err
	}

	getenv := func(key string) string {
		return map[string]string{"XDG_STATE_HOME": "/xdg/state"}[key]
	}

	cases := []configCase{}

	for _, path := range files {
		cfg, err := config.Load(getenv, path)
		item := configCase{File: filepath.Base(path)}

		switch {
		case err == nil:
			item.Config = summarize(cfg)
		case strings.HasPrefix(err.Error(), "invalid config"):
			item.Error = err.Error()
		default:
			item.Error = "parse"
		}

		cases = append(cases, item)
	}

	return cases, nil
}

func init() {
	// Paths with "~/" expand against a fixed home, so the output is the same everywhere.
	_ = os.Setenv("HOME", "/home/test")
}

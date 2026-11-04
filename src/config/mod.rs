//! Loads tamis's TOML configuration.

mod duration;
mod web;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;
use url::Url;

pub use web::Web;

const DEFAULT_PORT: u16 = 993;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_mins(2);
const DEFAULT_JOURNAL_RETENTION: Duration = Duration::from_hours(365 * 24);
const NO_HOME: &str = "no home directory: set XDG_CONFIG_HOME and XDG_STATE_HOME, pass --config, \
                       or set state in the config file";

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid config: {0}")]
    Invalid(String),
    #[error("read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("parse {path}: {message}")]
    Parse { path: String, message: String },
    #[error("{NO_HOME}")]
    NoHome,
    #[error("state: {NO_HOME}")]
    NoStateHome,
}

/// Looks up an environment variable; injected so tests need no real one.
pub type Getenv<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Says whether tamis acts on its decisions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Records what tamis would do and changes nothing.
    #[default]
    Shadow,
    /// Files messages according to their verdict.
    Live,
    /// Any other value; loading refuses it.
    #[serde(other)]
    Invalid,
}

/// The whole configuration file.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The file this was loaded from.
    #[serde(skip)]
    pub path: PathBuf,
    /// The `SQLite` database path.
    pub state: PathBuf,
    pub account: Account,
    pub folders: Folders,
    pub policy: Policy,
    pub rules: Rules,
    pub web: Web,
    pub monitor: Monitor,
}

/// Optional reporting to a dead-man's-switch service such as healthchecks.io.
/// The URLs are secrets: anyone who has one can fake a ping.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Monitor {
    /// The heartbeat check. Empty turns it off.
    pub ping_url: String,
    /// A second check, pinged once a day. Empty turns it off.
    pub quality_url: String,
    /// How many passes must fail in a row before `ping_url` is told of it.
    pub fail_after: u32,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            ping_url: String::new(),
            quality_url: String::new(),
            fail_after: 3,
        }
    }
}

impl Monitor {
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (key, value) in [
            ("ping_url", &self.ping_url),
            ("quality_url", &self.quality_url),
        ] {
            if !value.is_empty() && !is_http_url(value) {
                problems.push(format!("monitor.{key} must be an http(s) URL"));
            }
        }
        if self.fail_after < 1 {
            problems.push("monitor.fail_after must be at least 1".to_owned());
        }
        problems
    }
}

fn is_http_url(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| url.host_str().is_some() && matches!(url.scheme(), "http" | "https"))
}

/// The optional filing of good mail into folders.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// A Sieve (RFC 5228) file whose `fileinto` commands pick the folder good
    /// mail moves to. Empty files nothing.
    pub script: String,
    /// Makes a folder a rule files into when it is missing.
    pub create_folders: bool,
}

/// How to reach the mailbox.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Account {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Run without a shell; its trimmed stdout is the password.
    pub password_command: Vec<String>,
    /// The provider's authserv-id for its `Authentication-Results` headers.
    /// Left empty, tamis uses the domain bootstrap learnt; "none" turns
    /// pinning off.
    pub auth_server_id: String,
}

impl Default for Account {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT,
            user: String::new(),
            password_command: Vec::new(),
            auth_server_id: String::new(),
        }
    }
}

impl Account {
    /// The server's host:port.
    #[must_use]
    pub fn addr(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// The mailboxes tamis reads and writes.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Folders {
    pub watch: Vec<String>,
    pub junk: String,
    pub sent: String,
    pub train_spam: String,
    pub train_good: String,
    /// Where mail rescued from Junk goes; empty means the first watched folder.
    pub rescued: String,
}

impl Default for Folders {
    fn default() -> Self {
        Self {
            watch: vec!["INBOX".to_owned()],
            junk: "Junk".to_owned(),
            sent: "Sent Messages".to_owned(),
            train_spam: "TrainSpam".to_owned(),
            train_good: "TrainGood".to_owned(),
            rescued: String::new(),
        }
    }
}

impl Folders {
    /// The folder rescued mail is moved to.
    #[must_use]
    pub fn rescue_to(&self) -> &str {
        if self.rescued.is_empty() {
            &self.watch[0]
        } else {
            &self.rescued
        }
    }
}

/// Turns scores into actions.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is a switch in the file"
)]
pub struct Policy {
    pub mode: Mode,
    /// Scores at or above this are spam.
    pub spam_threshold: f64,
    /// Scores at or below this are good; anything between is uncertain.
    pub good_threshold: f64,
    #[serde(deserialize_with = "duration::deserialize")]
    pub poll_interval: Duration,
    /// Only trust a whitelisted sender when DKIM or DMARC passed.
    pub whitelist_requires_auth: bool,
    /// Treat a message tamis judged good or uncertain that later appears in
    /// Junk as missed spam.
    pub learn_from_junk: bool,
    /// Train mail the provider filed to Junk as spam when tamis scores it
    /// below `spam_threshold`.
    pub learn_provider_junk: bool,
    /// Learn from the Junk / Not Junk marks mail clients set.
    pub learn_from_keywords: bool,
    /// Move mail the provider filed to Junk back out when its sender is
    /// whitelisted and DKIM or DMARC passed.
    pub rescue: bool,
    /// How long decisions are kept. Zero keeps them forever.
    #[serde(deserialize_with = "duration::deserialize")]
    pub journal_retention: Duration,
    /// Also rescue mail scoring at or below this. Zero turns it off.
    pub rescue_threshold: f64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: Mode::Shadow,
            spam_threshold: 0.9,
            good_threshold: 0.2,
            poll_interval: DEFAULT_POLL_INTERVAL,
            whitelist_requires_auth: true,
            learn_from_junk: true,
            learn_provider_junk: true,
            learn_from_keywords: true,
            rescue: true,
            journal_retention: DEFAULT_JOURNAL_RETENTION,
            rescue_threshold: 0.0,
        }
    }
}

impl Policy {
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.mode == Mode::Invalid {
            problems.push(r#"policy.mode must be "shadow" or "live""#.to_owned());
        }
        if !(0.0 < self.good_threshold
            && self.good_threshold < self.spam_threshold
            && self.spam_threshold < 1.0)
        {
            problems.push("need 0 < policy.good_threshold < policy.spam_threshold < 1".to_owned());
        }
        if self.rescue_threshold < 0.0 || self.rescue_threshold >= self.good_threshold {
            problems.push("need 0 <= policy.rescue_threshold < policy.good_threshold".to_owned());
        }
        problems
    }
}

/// `$XDG_CONFIG_HOME/tamis/config.toml`, falling back to `~/.config`. `None`
/// when there is no home directory either.
#[must_use]
pub fn default_path(getenv: Getenv<'_>) -> Option<PathBuf> {
    let base = non_empty(getenv("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| Some(home(getenv)?.join(".config")))?;
    Some(base.join("tamis").join("config.toml"))
}

/// `$XDG_STATE_HOME/tamis/tamis.db`, falling back to `~/.local/state`. `None`
/// when there is no home directory either.
#[must_use]
pub fn default_state_path(getenv: Getenv<'_>) -> Option<PathBuf> {
    let base = non_empty(getenv("XDG_STATE_HOME"))
        .map(PathBuf::from)
        .or_else(|| Some(home(getenv)?.join(".local").join("state")))?;
    Some(base.join("tamis").join("tamis.db"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

fn home(getenv: Getenv<'_>) -> Option<PathBuf> {
    non_empty(getenv("HOME"))
        .or_else(|| non_empty(getenv("USERPROFILE")))
        .map(PathBuf::from)
}

fn expand_home(path: &str, getenv: Getenv<'_>) -> Result<String, Error> {
    let Some(rest) = path.strip_prefix("~/") else {
        return Ok(path.to_owned());
    };
    let home =
        home(getenv).ok_or_else(|| Error::Invalid(format!("expand {path}: no home directory")))?;
    Ok(home.join(rest).to_string_lossy().into_owned())
}

impl Config {
    /// Reads and validates the file at `path`, filling unset fields with
    /// defaults. Unknown keys are errors, so typos don't pass silently.
    ///
    /// # Errors
    /// Fails when the file can't be read, is writable by others, doesn't
    /// parse, or holds values tamis can't work with.
    pub fn load(getenv: Getenv<'_>, path: &Path) -> Result<Self, Error> {
        if path.as_os_str().is_empty() {
            return Err(Error::NoHome);
        }

        check_not_writable_by_others(path)?;
        let text = fs::read_to_string(path)?;

        let mut cfg = decode(path, &text)?;
        cfg.path = path.to_path_buf();
        cfg.validate()?;

        if cfg.state.as_os_str().is_empty() {
            cfg.state = default_state_path(getenv).ok_or(Error::NoStateHome)?;
        }
        cfg.state = expand_home(&cfg.state.to_string_lossy(), getenv)?.into();
        cfg.web.tls_cert = expand_home(&cfg.web.tls_cert, getenv)?;
        cfg.web.tls_key = expand_home(&cfg.web.tls_key, getenv)?;
        if !cfg.rules.script.is_empty() {
            cfg.rules.script = expand_home(&cfg.rules.script, getenv)?;
        }

        Ok(cfg)
    }

    fn validate(&self) -> Result<(), Error> {
        let mut problems = Vec::new();
        if self.account.host.is_empty() {
            problems.push("account.host is required, e.g. imap.mail.me.com for iCloud".to_owned());
        }
        if self.account.user.is_empty() {
            problems.push("account.user is required".to_owned());
        }
        if self.folders.watch.is_empty() {
            problems.push("folders.watch needs at least one folder".to_owned());
        }
        problems.extend(self.policy.problems());
        problems.extend(self.web.problems());
        problems.extend(self.monitor.problems());

        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(problems.join("; ")))
        }
    }
}

fn decode(path: &Path, text: &str) -> Result<Config, Error> {
    let mut unknown = Vec::new();
    let parsed: Result<Config, _> = serde_ignored::deserialize(
        toml::Deserializer::parse(text).map_err(|e| parse_error(path, &e))?,
        |key| {
            unknown.push(key.to_string());
        },
    );

    let cfg = parsed.map_err(|e| parse_error(path, &e))?;
    if unknown.is_empty() {
        Ok(cfg)
    } else {
        Err(Error::Invalid(format!(
            "unknown keys {}",
            unknown.join(", ")
        )))
    }
}

fn parse_error(path: &Path, err: &dyn std::fmt::Display) -> Error {
    Error::Parse {
        path: path.display().to_string(),
        message: err.to_string(),
    }
}

/// Refuses a config file other users could edit: `password_command` runs
/// whatever it names.
#[cfg(unix)]
fn check_not_writable_by_others(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o022 != 0 {
        let name = path.display();
        return Err(Error::Invalid(format!(
            "{name} is writable by others, and password_command runs what it names; fix with: chmod go-w {name}"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_not_writable_by_others(_path: &Path) -> Result<(), Error> {
    Ok(())
}

/// Whether `host` names the local machine.
pub(crate) fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The host part of a "host:port" address, or `None` when it is not one.
pub(crate) fn split_host_port(addr: &str) -> Option<&str> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        return tail.strip_prefix(':').map(|_| host);
    }
    let (host, port) = addr.split_once(':')?;
    (!port.contains(':')).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env(key: &str) -> Option<String> {
        match key {
            "XDG_CONFIG_HOME" => Some("/xdg/config".to_owned()),
            "HOME" => Some("/home/me".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn default_paths_follow_xdg_then_home() {
        assert_eq!(
            default_path(&env),
            Some(PathBuf::from("/xdg/config/tamis/config.toml"))
        );
        assert_eq!(
            default_state_path(&env),
            Some(PathBuf::from("/home/me/.local/state/tamis/tamis.db"))
        );
        assert_eq!(default_path(&no_env), None);
        assert_eq!(default_state_path(&no_env), None);
    }

    #[test]
    fn host_and_port_split_like_go() {
        let cases = [
            ("127.0.0.1:8484", Some("127.0.0.1")),
            ("[::1]:9000", Some("::1")),
            (":8484", Some("")),
            ("8484", None),
            ("a:b:c", None),
            ("[::1]", None),
        ];
        for (addr, want) in cases {
            assert_eq!(split_host_port(addr), want, "{addr}");
        }
    }

    #[test]
    fn ipv6_servers_are_bracketed() {
        let account = Account {
            host: "::1".to_owned(),
            ..Account::default()
        };
        assert_eq!(account.addr(), "[::1]:993");
    }

    #[test]
    fn an_empty_path_means_no_home() {
        let err = Config::load(&no_env, Path::new("")).unwrap_err();
        assert!(matches!(err, Error::NoHome));
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let err = Config::load(&no_env, Path::new("/nonexistent/tamis.toml")).unwrap_err();
        assert!(err.to_string().starts_with("read config:"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[account]\nhost = \"h\"\nuser = \"u\"\n").unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Config::load(&env, &path).is_ok());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let err = Config::load(&env, &path).unwrap_err().to_string();
        assert!(err.contains("is writable by others"), "{err}");
    }

    #[test]
    fn a_state_path_needs_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[account]\nhost = \"h\"\nuser = \"u\"\n").unwrap();

        let err = Config::load(&no_env, &path).unwrap_err();
        assert!(matches!(err, Error::NoStateHome));
    }
}

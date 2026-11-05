//! The tamis command-line interface.

mod commands;
pub mod logging;
mod password;
pub mod style;
#[cfg(test)]
mod testing;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use crate::classify::Label;
use crate::config;
use crate::secret::Keyring;
use password::PasswordFinder;

/// Exit codes.
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
/// A bad command line: nothing ran.
pub const EXIT_USAGE: i32 = 2;

/// Why a prompt for a secret gave none.
#[derive(Debug)]
pub enum PromptError {
    /// There is no terminal to ask on.
    NotTerminal,
    Other(String),
}

/// Asks for a secret without echoing it.
pub type Prompt = Box<dyn Fn(&str) -> Result<String, PromptError> + Send + Sync>;
pub type Getenv = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// Starts logging: whether `--verbose` was given, and `$JOURNAL_STREAM`.
pub type LogInit = Box<dyn Fn(bool, Option<String>) + Send + Sync>;

/// The process environment [`run`] works against.
pub struct Env {
    pub stdin: Box<dyn BufRead + Send>,
    pub stdout: Box<dyn Write + Send>,
    pub stderr: Box<dyn Write + Send>,
    pub getenv: Getenv,
    /// Whether `stdout` is a terminal, for colour.
    pub stdout_is_terminal: bool,
    /// Asks for a secret without echoing it, if there is a way to.
    pub prompt: Option<Prompt>,
    /// The system keychain, or `None` where there is none.
    pub keyring: Option<Arc<dyn Keyring>>,
    /// Dials IMAP without TLS. Only for local test servers.
    pub plaintext_imap: bool,
    pub version: String,
    /// Sets up logging once the command line is read; `None` logs nowhere.
    pub logging: Option<LogInit>,
}

impl Env {
    /// Takes what a command that outlives the call needs to find a password
    /// again later: standard input, the prompt, the keychain and the
    /// variables. The command keeps its standard output and error.
    pub(crate) fn take_credentials(&mut self) -> Self {
        Self {
            stdin: std::mem::replace(&mut self.stdin, Box::new(std::io::empty())),
            stdout: Box::new(std::io::sink()),
            stderr: Box::new(std::io::sink()),
            getenv: Arc::clone(&self.getenv),
            stdout_is_terminal: false,
            prompt: self.prompt.take(),
            keyring: self.keyring.take(),
            plaintext_imap: self.plaintext_imap,
            version: self.version.clone(),
            logging: None,
        }
    }
}

/// Marks an error the command already told the user about.
#[derive(Debug)]
pub struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("command failed")
    }
}

impl std::error::Error for Reported {}

#[allow(clippy::doc_markdown, reason = "these doc comments are the help text")]
#[derive(Parser, Debug)]
#[command(
    name = "tamis",
    about = "A second-opinion spam filter and mail sorter for remote mailboxes",
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// Print the version
    #[arg(short = 'V', long = "version", global = false)]
    version: bool,
    /// Log each IMAP exchange
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Colour output: auto (a terminal, unless NO_COLOR is set), always or never
    #[arg(long, global = true, default_value = "auto")]
    color: String,
    /// Configuration file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Args, Debug)]
pub(crate) struct ProbeArgs {
    /// IMAP server, e.g. imap.mail.me.com for iCloud
    #[arg(long)]
    host: Option<String>,
    /// IMAP port (implicit TLS)
    #[arg(long)]
    port: Option<u16>,
    /// Login name, usually the full address
    #[arg(long)]
    user: Option<String>,
    /// Also create, fill and delete scratch folders
    #[arg(long)]
    write: bool,
    /// Print the report as JSON
    #[arg(long)]
    json: bool,
    /// How long to wait for an IDLE push
    #[arg(long, default_value = "30s")]
    idle_wait: String,
    /// Connect without TLS (local test servers only)
    #[arg(long)]
    plaintext: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Write a starter configuration file
    #[command(
        long_about = "Write a configuration file naming the account. Every other setting has a\ndefault. It refuses to overwrite an existing file."
    )]
    Init {
        /// IMAP server, e.g. imap.mail.me.com for iCloud
        #[arg(long)]
        host: String,
        /// Login name, usually the full address
        #[arg(long)]
        user: String,
    },
    /// Check which IMAP features a server supports
    #[command(
        long_about = "Check which IMAP features a server supports, and list its folders.\n\n--host, --user and --port default to the account in the configuration file.\nThe password (an app-specific one for iCloud) comes from the same places as\nfor `tamis run`."
    )]
    Probe(ProbeArgs),
    /// Check the account password and keep it in the system keychain
    #[command(
        long_about = "Ask for the account password (an app-specific one for iCloud), check it\nagainst the server, and store it in the system keychain: the macOS Keychain,\nthe Secret Service (GNOME Keyring, KWallet) or the Windows Credential Manager."
    )]
    Login,
    /// Remove the account password from the system keychain
    Logout,
    /// Seed the whitelist and the filter from existing mail
    #[command(
        long_about = "Seed the whitelist from Sent recipients and the filter from Junk (as spam) and\nold read inbox mail (as good), then start watching from current mail."
    )]
    Bootstrap(BootstrapArgs),
    /// Judge new mail, then keep watching
    #[command(
        long_about = "Train from the training folders and judge new mail in the watched folders,\nevery policy.poll_interval until interrupted. In shadow mode decisions are only\nrecorded; see them with 'tamis log'."
    )]
    Run {
        /// Make a single pass and exit
        #[arg(long)]
        once: bool,
    },
    /// Reverse decisions tamis applied
    #[command(
        long_about = "Reverse decisions tamis applied, by the IDs 'tamis log' shows. Mail filed\nto Junk is moved back, learnt as good and its sender whitelisted; an\nuncertain flag is cleared."
    )]
    Undo {
        #[arg(required = true)]
        ids: Vec<i64>,
    },
    /// Teach tamis the right answer for decisions it only recorded
    #[command(
        long_about = "Teach tamis the right answer for decisions it recorded without acting on, or\nflagged uncertain, by the IDs 'tamis log' shows. With no flag it learns the opposite\nof the verdict. The mail stays where it is, in its folder or in Junk, except an uncertain\none: its flag is cleared, and it moves to Junk if learnt as spam. A\ncorrection can be learnt again to change it. Use 'tamis undo' for other\ndecisions that were applied."
    )]
    Learn {
        #[arg(required = true)]
        ids: Vec<i64>,
        /// Learn as good
        #[arg(long, conflicts_with = "spam")]
        good: bool,
        /// Learn as spam
        #[arg(long)]
        spam: bool,
    },
    /// Show recent decisions
    #[command(
        long_about = "Show recent decisions, newest first. In shadow mode, actions read\n\"would ...\": what live mode would have done."
    )]
    Log {
        /// How many decisions to show
        #[arg(short = 'n', long = "number", default_value_t = 20)]
        number: i64,
    },
    /// Show the configuration and what has been learnt
    Status,
    /// Check the configuration, the rules and that the filter is in good shape
    #[command(
        long_about = "Check that the configuration loads and the filing rules compile, that the filter\nhas learnt from both kinds of mail, how it has been deciding lately, and whether\nit is still deciding. Exits non-zero when a check fails."
    )]
    Doctor {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the checks to set up on a monitoring service such as healthchecks.io
    #[command(
        long_about = "Work out, from poll_interval and monitor.fail_after, the period and grace time of\nthe two checks `tamis run` pings when [monitor] is configured, and how soon each\nkind of trouble would alert."
    )]
    Monitor {
        /// Print the settings as JSON
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<MonitorAction>,
    },
    /// Manage passkeys for the web interface
    #[command(
        long_about = "The web interface (the [web] config section, served by `tamis run`) signs in\nwith passkeys. Enroll the first one with `tamis web enroll`."
    )]
    Web {
        #[command(subcommand)]
        action: WebAction,
    },
    /// View and edit the whitelist
    #[command(
        long_about = "The whitelist holds senders tamis trusts. Recipients you write to are\nadded automatically; these commands add and remove addresses by hand."
    )]
    Whitelist {
        #[command(subcommand)]
        action: WhitelistAction,
    },
    /// View, validate and try the filing rules
    #[command(
        long_about = "Good mail is filed into folders by a Sieve script (RFC 5228) named by\nrules.script. Only fileinto matters: tamis moves mail over IMAP."
    )]
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// Print the version
    Version,
}

/// What `bootstrap` learns from.
#[derive(clap::Args, Debug)]
pub(crate) struct BootstrapArgs {
    /// Recent sent messages to whitelist recipients from
    #[arg(long)]
    pub sent: Option<usize>,
    /// FOLDER=N: train the latest N messages as spam (default: junk folder=500)
    #[arg(short = 's', long = "spam")]
    pub spam: Vec<String>,
    /// FOLDER=N: train the latest N read messages as good (default: first watched folder=500)
    #[arg(short = 'g', long = "good")]
    pub good: Vec<String>,
    /// Only train read mail at least this old as good
    #[arg(long, default_value = "168h")]
    pub good_min_age: String,
    /// Learn already-learnt mail again with the current tokenizer; leaves the whitelist and watch position alone
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Subcommand, Debug)]
enum MonitorAction {
    /// Send one ping to each configured check, to see that the URLs work
    Test,
}

#[derive(Subcommand, Debug)]
enum WebAction {
    /// Print a one-time link to enroll a passkey
    Enroll,
    /// List enrolled passkeys
    Passkeys,
    /// Show the certificate to install on your devices
    #[command(
        long_about = "Show the certificate the web interface serves HTTPS with, making a self-signed\none first if web.self_signed is set. Browsers only accept passkeys from a\ncertificate they trust, so install it on each device. --pem prints the\ncertificate itself, never the key."
    )]
    Cert {
        /// Print the certificate as PEM, for copying to a device
        #[arg(long)]
        pem: bool,
    },
    /// Remove enrolled passkeys
    Revoke {
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
}

#[derive(Subcommand, Debug)]
enum WhitelistAction {
    /// List whitelisted addresses
    List,
    /// Whitelist addresses
    Add {
        #[arg(required = true)]
        addresses: Vec<String>,
    },
    /// Remove addresses from the whitelist
    #[command(
        visible_alias = "remove",
        long_about = "Remove the given addresses, or with --matching every address or source that\ncontains TEXT. --matching lists what it would remove and does nothing more\nuntil you repeat it with --yes."
    )]
    Rm {
        #[arg(required_unless_present = "matching", conflicts_with = "matching")]
        addresses: Vec<String>,
        /// Remove every address or source containing this text
        #[arg(long)]
        matching: Option<String>,
        /// With --matching, really remove them
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum RulesAction {
    /// Print the configured rules script
    Show,
    /// Validate a Sieve script (default: rules.script)
    #[command(
        long_about = "Compile the script and report any error. With --folders, also connect to the\nmailbox and fail if a folder the script files into doesn't exist."
    )]
    Check {
        file: Option<PathBuf>,
        /// Also check the folders the script files into exist in the mailbox
        #[arg(long)]
        folders: bool,
    },
    /// Show where the rules would file a message
    Test { message: PathBuf },
}

/// What a command works with.
#[allow(
    dead_code,
    reason = "the commands that connect to the mailbox, which come next, use these"
)]
pub(crate) struct App {
    pub env: Env,
    pub config_path: PathBuf,
    pub color: String,
    pub verbose: bool,
    pub finder: PasswordFinder,
    #[allow(dead_code, reason = "commands that run until interrupted stop on it")]
    pub shutdown: CancellationToken,
}

/// Executes the command line in `args` (without the program name) and returns
/// the process exit code.
pub async fn run(args: &[String], mut env: Env, shutdown: CancellationToken) -> i32 {
    let program_and_args = std::iter::once("tamis".to_owned()).chain(args.iter().cloned());
    let cli = match Cli::try_parse_from(program_and_args) {
        Ok(cli) => cli,
        Err(err) => {
            let text = err.render().to_string();
            return if err.use_stderr() {
                let _ = env.stderr.write_all(text.as_bytes());
                EXIT_USAGE
            } else {
                let _ = env.stdout.write_all(text.as_bytes());
                EXIT_OK
            };
        }
    };

    if let Some(init) = &env.logging {
        init(cli.verbose, (env.getenv)("JOURNAL_STREAM"));
    }

    let config_path = cli
        .config
        .clone()
        .or_else(|| config::default_path(&*env.getenv))
        .unwrap_or_default();
    let mut app = App {
        env,
        config_path,
        color: cli.color.clone(),
        verbose: cli.verbose,
        finder: PasswordFinder::default(),
        shutdown,
    };

    let outcome = dispatch(&mut app, cli).await;
    match outcome {
        Ok(()) => EXIT_OK,
        Err(err) => {
            if err.downcast_ref::<Reported>().is_none() {
                let _ = writeln!(app.env.stderr, "tamis: {err:#}");
            }
            EXIT_FAIL
        }
    }
}

async fn dispatch(app: &mut App, cli: Cli) -> Result<()> {
    if cli.version {
        return commands::version(app);
    }
    let Some(command) = cli.command else {
        let _ = Cli::parse_from(["tamis", "--help"]);
        return Ok(());
    };
    match command {
        Command::Version => commands::version(app),
        Command::Init { host, user } => commands::init(app, &host, &user),
        Command::Probe(args) => commands::probe(app, &args).await,
        Command::Login => commands::login(app).await,
        Command::Logout => commands::logout(app),
        Command::Bootstrap(args) => commands::bootstrap(app, &args).await,
        Command::Run { once } => commands::run(app, once).await,
        Command::Undo { ids } => commands::undo(app, &ids).await,
        Command::Learn { ids, good, spam } => {
            let label = if good {
                Some(Label::Good)
            } else if spam {
                Some(Label::Spam)
            } else {
                None
            };
            commands::learn(app, &ids, label).await
        }
        Command::Log { number } => commands::log(app, number),
        Command::Status => commands::status(app),
        Command::Doctor { json } => commands::doctor(app, json),
        Command::Monitor { json, action: None } => commands::monitor(app, json),
        Command::Monitor {
            action: Some(MonitorAction::Test),
            ..
        } => commands::monitor_test(app).await,
        Command::Web { action } => match action {
            WebAction::Enroll => commands::web_enroll(app),
            WebAction::Passkeys => commands::web_passkeys(app),
            WebAction::Cert { pem } => commands::web_cert(app, pem),
            WebAction::Revoke { ids } => commands::web_revoke(app, &ids),
        },
        Command::Whitelist { action } => match action {
            WhitelistAction::List => commands::whitelist_list(app),
            WhitelistAction::Add { addresses } => commands::whitelist_add(app, &addresses),
            WhitelistAction::Rm {
                addresses,
                matching,
                yes,
            } => commands::whitelist_remove(app, &addresses, matching.as_deref(), yes),
        },
        Command::Rules { action } => match action {
            RulesAction::Show => commands::rules_show(app),
            RulesAction::Check { file, folders } => {
                commands::rules_check(app, file.as_deref(), folders).await
            }
            RulesAction::Test { message } => commands::rules_test(app, &message),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::testing::{Harness, imap_server};
    use crate::config::Config;
    use crate::secret::{Keyring, service};

    const NO_ARGS: &[&str] = &[];

    #[tokio::test]
    async fn the_version_is_printed_by_the_command_and_the_flag() {
        for args in [&["version"][..], &["--version"][..], &["-V"][..]] {
            let (code, out, _) = Harness::new(&[], "").run(args).await;

            assert_eq!(
                (code, out.text().as_str()),
                (0, "tamis 1.2.3\n"),
                "{args:?}"
            );
        }
    }

    #[tokio::test]
    async fn help_goes_to_stdout_and_a_bad_command_line_is_a_usage_error() {
        let (code, out, _) = Harness::new(&[], "").run(&["--help"]).await;
        assert_eq!(code, 0);
        assert!(
            out.text().contains("A second-opinion spam filter"),
            "{}",
            out.text()
        );

        for args in [
            &["frobnicate"][..],
            &["init", "--host", "h"][..],
            &["--color"][..],
        ] {
            let (code, out, err) = Harness::new(&[], "").run(args).await;
            assert_eq!(code, 2, "{args:?}");
            assert!(out.text().is_empty() && !err.text().is_empty(), "{args:?}");
        }

        let (code, ..) = Harness::new(&[], "").run(NO_ARGS).await;
        assert_eq!(code, 0, "no command shows the help");
    }

    #[tokio::test]
    async fn init_writes_a_private_config_that_loads_and_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let path_arg = path.to_str().unwrap();

        let (code, out, _) = Harness::new(&[], "")
            .run(&[
                "--config",
                path_arg,
                "init",
                "--host",
                "imap.example.org",
                "--user",
                "me@ex\"ample.org",
            ])
            .await;

        assert_eq!(code, 0);
        assert!(
            out.text().contains("Wrote ") && out.text().contains("tamis login"),
            "{}",
            out.text()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let getenv = |key: &str| {
            (key == "XDG_STATE_HOME").then(|| dir.path().to_string_lossy().into_owned())
        };
        let cfg = Config::load(&getenv, &path).unwrap();
        assert_eq!(
            (cfg.account.host.as_str(), cfg.account.user.as_str()),
            ("imap.example.org", "me@ex\"ample.org")
        );

        let (code, _, err) = Harness::new(&[], "")
            .run(&["--config", path_arg, "init", "--host", "x", "--user", "y"])
            .await;
        assert_eq!(code, 1);
        assert!(
            err.text()
                .contains("config already exists; edit it instead"),
            "{}",
            err.text()
        );
    }

    fn config_for(dir: &std::path::Path, port: u16) -> String {
        let path = dir.join("config.toml");
        std::fs::write(&path, format!("state = \"{}\"\n[account]\nhost = \"127.0.0.1\"\nport = {port}\nuser = \"me@example.org\"\n", dir.join("s.db").display())).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn login_checks_the_password_and_keeps_it_in_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_for(dir.path(), imap_server(true).await);
        let harness = Harness::new(&[], "s3cret\n");
        let keyring = std::sync::Arc::clone(&harness.keyring);

        let (code, out, err) = harness.run(&["-c", &path, "login"]).await;

        assert_eq!((code, err.text().as_str()), (0, ""));
        assert_eq!(
            out.text(),
            "Saved the password for me@example.org on 127.0.0.1 in the system keychain.\n"
        );
        assert_eq!(
            keyring
                .get(&service("127.0.0.1"), "me@example.org")
                .unwrap(),
            "s3cret"
        );
    }

    #[tokio::test]
    async fn a_rejected_password_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_for(dir.path(), imap_server(false).await);
        let harness = Harness::new(&[], "wrong\n");
        let keyring = std::sync::Arc::clone(&harness.keyring);

        let (code, _, err) = harness.run(&["-c", &path, "login"]).await;

        assert_eq!(code, 1);
        assert!(err.text().contains("check the password"), "{}", err.text());
        assert!(
            keyring
                .get(&service("127.0.0.1"), "me@example.org")
                .is_err()
        );
    }

    #[tokio::test]
    async fn login_and_logout_need_a_keychain_and_a_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_for(dir.path(), 1);

        let (code, _, err) = Harness::new(&[], "x\n")
            .without_keyring()
            .run(&["-c", &path, "login"])
            .await;
        assert_eq!(code, 1);
        assert!(
            err.text()
                .contains("no system keychain available; use account.password_command"),
            "{}",
            err.text()
        );

        let (code, _, err) = Harness::new(&[], "").run(&["-c", &path, "login"]).await;
        assert_eq!(code, 1);
        assert!(
            err.text().contains("no password: run 'tamis login'"),
            "{}",
            err.text()
        );
    }

    #[tokio::test]
    async fn logout_removes_the_password_and_says_when_there_was_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_for(dir.path(), 1);
        let harness = Harness::new(&[], "");
        harness
            .keyring
            .set(&service("127.0.0.1"), "me@example.org", "pw")
            .unwrap();
        let keyring = std::sync::Arc::clone(&harness.keyring);

        let (code, out, _) = harness.run(&["-c", &path, "logout"]).await;
        assert_eq!(code, 0);
        assert_eq!(
            out.text(),
            "Removed the password for me@example.org on 127.0.0.1 from the system keychain.\n"
        );
        assert!(
            keyring
                .get(&service("127.0.0.1"), "me@example.org")
                .is_err()
        );

        let (code, out, _) = Harness::new(&[], "").run(&["-c", &path, "logout"]).await;
        assert_eq!(code, 0);
        assert_eq!(
            out.text(),
            "No password stored for me@example.org on 127.0.0.1.\n"
        );
    }

    #[tokio::test]
    async fn a_missing_configuration_is_reported_by_name() {
        let (code, _, err) = Harness::new(&[], "")
            .run(&["-c", "/no/such/config.toml", "logout"])
            .await;

        assert_eq!(code, 1);
        assert!(
            err.text().starts_with("tamis: read config:"),
            "{}",
            err.text()
        );
    }

    #[tokio::test]
    async fn a_failing_run_still_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = config_for(dir.path(), 1);

        let (code, _, err) = Harness::new(&[], "")
            .run(&["-c", &path, "run", "--once"])
            .await;

        assert_eq!(code, 1);
        assert!(
            err.text().contains("no password: run 'tamis login'"),
            "{}",
            err.text()
        );
    }
}

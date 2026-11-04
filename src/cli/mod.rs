//! The tamis command-line interface.

mod commands;
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
pub type Getenv = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

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
    /// Check the account password and keep it in the system keychain
    #[command(
        long_about = "Ask for the account password (an app-specific one for iCloud), check it\nagainst the server, and store it in the system keychain: the macOS Keychain,\nthe Secret Service (GNOME Keyring, KWallet) or the Windows Credential Manager."
    )]
    Login,
    /// Remove the account password from the system keychain
    Logout,
    /// Print the version
    Version,
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
        Command::Login => commands::login(app).await,
        Command::Logout => commands::logout(app),
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
}

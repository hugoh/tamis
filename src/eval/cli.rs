//! `tamis-eval fetch` and `tamis-eval run`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};

use super::{DEFAULT_WARMUP, Options, compare, load, standard, write_report};
use crate::config::{Config, default_path};

const DEFAULT_RUNS: usize = 5;

#[derive(Parser, Debug)]
#[command(
    name = "tamis-eval",
    about = "Compare ways of tokenizing mail on a corpus of your own"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Copy the latest messages of some folders to a directory outside the repository
    Fetch {
        /// tamis configuration file
        #[arg(long)]
        config: Option<PathBuf>,
        /// Folder=N: the latest N messages of Folder are spam (repeatable)
        #[arg(long, required = true)]
        spam: Vec<String>,
        /// Folder=N: the latest N messages of Folder are good (repeatable)
        #[arg(long, required = true)]
        ham: Vec<String>,
        /// Where to write ham/ and spam/
        dir: PathBuf,
    },
    /// Replay a corpus through each variant and compare them
    Run {
        /// Runs per variant: the first in order, the others shuffled
        #[arg(long, default_value_t = DEFAULT_RUNS)]
        runs: usize,
        /// Leading messages learnt but not scored
        #[arg(long, default_value_t = DEFAULT_WARMUP)]
        warmup: usize,
        /// Comma-separated addresses mail is always expected on (default: the tamis account's, if there is a config)
        #[arg(long)]
        own: Option<String>,
        /// The folder holding ham/ and spam/
        dir: PathBuf,
    },
}

/// Runs the tool with the arguments after its name.
///
/// # Errors
/// What went wrong, for the caller to print.
pub async fn run(
    args: &[String],
    out: &mut impl Write,
    getenv: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<()> {
    let cli =
        Cli::try_parse_from(std::iter::once("tamis-eval".to_owned()).chain(args.iter().cloned()))?;
    match cli.command {
        Command::Run {
            runs,
            warmup,
            own,
            dir,
        } => run_command(out, getenv, runs, warmup, own.as_deref(), &dir),
        Command::Fetch {
            config,
            spam,
            ham,
            dir,
        } => super::net::fetch_command(out, getenv, config, &spam, &ham, &dir).await,
    }
}

fn run_command(
    out: &mut impl Write,
    getenv: &(dyn Fn(&str) -> Option<String> + Sync),
    runs: usize,
    warmup: usize,
    own: Option<&str>,
    dir: &Path,
) -> Result<()> {
    if runs == 0 {
        bail!("--runs must be at least 1");
    }
    let messages = load(dir).context("run")?;
    let spam = messages.iter().filter(|m| m.spam).count();
    writeln!(
        out,
        "{} good and {spam} spam messages\n",
        messages.len() - spam
    )?;

    let variants = standard(&own_addresses(own, getenv));
    let results = compare(&messages, &variants, runs, Options { warmup }).context("run")?;
    write_report(out, &variants, &results)?;
    Ok(())
}

/// The addresses mail is always expected on: the flag's, else the account's.
fn own_addresses(
    flag: Option<&str>,
    getenv: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Vec<String> {
    if let Some(flag) = flag.filter(|f| !f.is_empty()) {
        return flag.split(',').map(str::to_owned).collect();
    }
    default_path(getenv)
        .and_then(|path| Config::load(getenv, &path).ok())
        .map_or_default(|cfg| vec![cfg.account.user])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    fn corpus(dir: &Path) {
        for (class, subject, body) in [
            ("ham", "lunch", "lunch tomorrow meeting agenda"),
            ("spam", "pills", "cheap pills winner prize"),
        ] {
            std::fs::create_dir_all(dir.join(class)).unwrap();
            for n in 0..30 {
                std::fs::write(
                    dir.join(class).join(format!("{n}.eml")),
                    format!("From: a{n}@x.example\r\nTo: me@example.org\r\nDate: Mon, 01 Jan 2024 10:{n:02}:00 +0000\r\nSubject: {subject} {n}\r\n\r\n{body}"),
                )
                .unwrap();
            }
        }
    }

    async fn eval(
        args: &[&str],
        getenv: &(dyn Fn(&str) -> Option<String> + Sync),
    ) -> (Result<()>, String) {
        let mut out = Vec::new();
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let result = run(&args, &mut out, getenv).await;
        (result, String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn run_reports_the_corpus_and_compares_the_variants() {
        let dir = tempfile::tempdir().unwrap();
        corpus(dir.path());

        let (result, out) = eval(
            &[
                "run",
                "--runs",
                "2",
                "--warmup",
                "10",
                "--own",
                "me@example.org",
                dir.path().to_str().unwrap(),
            ],
            &none,
        )
        .await;

        result.unwrap();
        assert!(
            out.starts_with("30 good and 30 spam messages\n\nvariant"),
            "{out}"
        );
        assert!(
            out.contains("baseline (tamis today)") && out.contains("- recipient tokens"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn run_needs_a_corpus_and_sensible_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let (empty, _) = eval(&["run", path], &none).await;
        let (zero, _) = eval(&["run", "--runs", "0", path], &none).await;
        let (unknown, _) = eval(&["frobnicate"], &none).await;

        assert!(empty.unwrap_err().to_string().contains("run"));
        assert!(zero.unwrap_err().to_string().contains("--runs"));
        assert!(unknown.is_err());
    }

    #[test]
    fn own_addresses_come_from_the_flag_or_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("tamis").join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "state = \"/tmp/x.db\"\n[account]\nhost = \"imap.example.org\"\nuser = \"me@example.org\"\n").unwrap();
        let home = dir.path().to_str().unwrap().to_owned();
        let getenv = move |key: &str| (key == "XDG_CONFIG_HOME").then(|| home.clone());

        assert_eq!(
            own_addresses(Some("a@x.org,b@x.org"), &getenv),
            ["a@x.org", "b@x.org"]
        );
        assert_eq!(own_addresses(None, &getenv), ["me@example.org"]);
        assert!(own_addresses(None, &none).is_empty());
    }
}

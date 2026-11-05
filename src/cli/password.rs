#![allow(
    dead_code,
    reason = "the commands that connect to the mailbox, which come next, use the finder"
)]

use std::io::BufRead;
use std::path::Path;

use anyhow::{Context as _, Result, anyhow, bail};

use super::{Env, PromptError};
use crate::config::{Account, PasswordError};
use crate::secret;

const PASSWORD_ENV: &str = "TAMIS_PASSWORD";
/// The systemd credential tamis reads, e.g. from
/// `LoadCredentialEncrypted=password:/etc/credstore.encrypted/tamis`.
const CREDENTIAL_NAME: &str = "password";

pub(super) fn no_password() -> anyhow::Error {
    anyhow!("no password: run 'tamis login', or set account.password_command or ${PASSWORD_ENV}")
}

/// Whose password to find.
pub(crate) struct Who<'a> {
    pub host: &'a str,
    pub user: &'a str,
    pub command: &'a [String],
}

/// Looks for the password in, by order of preference:
///
/// 1. `account.password_command`, when configured;
/// 2. the systemd credential `$CREDENTIALS_DIRECTORY/password`;
/// 3. the system keychain (see `tamis login`);
/// 4. `$TAMIS_PASSWORD`, which other processes can read: a last resort;
/// 5. typing it, on a terminal, or on stdin when there is no terminal.
///
/// A typed password is kept, so a long-running command asks only once.
#[derive(Default)]
pub(crate) struct PasswordFinder {
    typed: Option<String>,
    /// What `password_command` printed, so a long-running command doesn't call
    /// the vault on every pass.
    command: Option<String>,
}

impl PasswordFinder {
    pub(super) async fn find(&mut self, env: &mut Env, who: &Who<'_>) -> Result<String> {
        if !who.command.is_empty() {
            return self.run_command(who.command).await;
        }
        if let Some(found) = systemd_credential(env)? {
            return Ok(found);
        }
        if let Some(keyring) = &env.keyring {
            // An unreachable keychain (a headless server) falls through.
            if let Ok(password) = keyring.get(&secret::service(who.host), who.user) {
                return Ok(password);
            }
        }
        if let Some(password) = (env.getenv)(PASSWORD_ENV).filter(|p| !p.is_empty()) {
            return Ok(password);
        }
        if let Some(typed) = &self.typed {
            return Ok(typed.clone());
        }
        let password = type_password(env, who.user)?;
        self.typed = Some(password.clone());
        Ok(password)
    }

    async fn run_command(&mut self, command: &[String]) -> Result<String> {
        if let Some(cached) = &self.command {
            return Ok(cached.clone());
        }
        let account = Account {
            password_command: command.to_vec(),
            ..Account::default()
        };
        let password = account
            .password()
            .await
            .map_err(|err: PasswordError| anyhow!("password: {err}"))?;
        self.command = Some(password.clone());
        Ok(password)
    }
}

/// Reads the password systemd passes a service through `LoadCredential=` or
/// `LoadCredentialEncrypted=`.
fn systemd_credential(env: &Env) -> Result<Option<String>> {
    let Some(dir) = (env.getenv)("CREDENTIALS_DIRECTORY").filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    match std::fs::read_to_string(Path::new(&dir).join(CREDENTIAL_NAME)) {
        Ok(content) => Ok(Some(content.trim_end_matches(['\r', '\n']).to_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("read systemd credential"),
    }
}

/// Asks on the terminal, or reads the first line of stdin if there is no
/// terminal.
pub(super) fn type_password(env: &mut Env, user: &str) -> Result<String> {
    if let Some(prompt) = &env.prompt {
        match prompt(&format!("Password for {user}: ")) {
            Err(PromptError::NotTerminal) => {}
            Err(PromptError::Other(err)) => bail!("read password: {err}"),
            Ok(password) if password.is_empty() => return Err(no_password()),
            Ok(password) => return Ok(password),
        }
    }

    let mut line = String::new();
    env.stdin.read_line(&mut line).context("read password")?;
    let password = line.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        Err(no_password())
    } else {
        Ok(password.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::super::PromptError;
    use super::super::testing::Harness;
    use super::*;
    use crate::secret::Keyring;

    fn who(command: &[String]) -> Who<'_> {
        Who {
            host: "imap.example.org",
            user: "me@example.org",
            command,
        }
    }

    fn keychain_holds_a_password(harness: &Harness) {
        harness
            .keyring
            .set(
                &secret::service("imap.example.org"),
                "me@example.org",
                "from keychain",
            )
            .unwrap();
    }

    async fn found(harness: &mut Harness) -> String {
        PasswordFinder::default()
            .find(&mut harness.env, &who(&[]))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_password_command_comes_first_and_is_remembered() {
        let mut harness = Harness::new(&[("TAMIS_PASSWORD", "from env")], "typed\n");
        keychain_holds_a_password(&harness);
        let mut finder = PasswordFinder::default();
        let command = ["printf".to_owned(), "from command".to_owned()];

        assert_eq!(
            finder.find(&mut harness.env, &who(&command)).await.unwrap(),
            "from command"
        );

        let broken = ["false".to_owned()];
        assert_eq!(
            finder.find(&mut harness.env, &who(&broken)).await.unwrap(),
            "from command",
            "the vault isn't called on every pass"
        );
    }

    #[tokio::test]
    async fn a_failing_password_command_is_an_error_not_a_fallback() {
        let mut harness = Harness::new(&[("TAMIS_PASSWORD", "from env")], "");
        let broken = ["false".to_owned()];

        let err = PasswordFinder::default()
            .find(&mut harness.env, &who(&broken))
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .starts_with("password: account.password_command"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_systemd_credential_beats_the_keychain_and_a_missing_one_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("password"), "from systemd\r\n").unwrap();
        let credentials = dir.path().to_string_lossy().into_owned();

        let mut harness = Harness::new(&[("CREDENTIALS_DIRECTORY", &credentials)], "");
        keychain_holds_a_password(&harness);
        assert_eq!(found(&mut harness).await, "from systemd");

        let empty = tempfile::tempdir().unwrap();
        let credentials = empty.path().to_string_lossy().into_owned();
        let mut harness = Harness::new(&[("CREDENTIALS_DIRECTORY", &credentials)], "");
        keychain_holds_a_password(&harness);
        assert_eq!(found(&mut harness).await, "from keychain");
    }

    #[tokio::test]
    async fn the_keychain_beats_the_environment_which_beats_typing() {
        let mut harness = Harness::new(&[("TAMIS_PASSWORD", "from env")], "typed\n");
        assert_eq!(found(&mut harness).await, "from env");

        keychain_holds_a_password(&harness);
        assert_eq!(found(&mut harness).await, "from keychain");
    }

    #[tokio::test]
    async fn a_typed_password_is_asked_for_once() {
        let mut harness = Harness::new(&[], "first\nsecond\n");
        let mut finder = PasswordFinder::default();

        assert_eq!(
            finder.find(&mut harness.env, &who(&[])).await.unwrap(),
            "first"
        );
        assert_eq!(
            finder.find(&mut harness.env, &who(&[])).await.unwrap(),
            "first"
        );
    }

    #[tokio::test]
    async fn the_prompt_is_used_and_falls_back_to_stdin_without_a_terminal() {
        let mut harness = Harness::new(&[], "from stdin\n");
        harness.env.prompt = Some(Box::new(|text| {
            assert_eq!(text, "Password for me@example.org: ");
            Ok("from prompt".to_owned())
        }));
        assert_eq!(found(&mut harness).await, "from prompt");

        harness.env.prompt = Some(Box::new(|_| Err(PromptError::NotTerminal)));
        assert_eq!(found(&mut harness).await, "from stdin");

        harness.env.prompt = Some(Box::new(|_| Err(PromptError::Other("broken".into()))));
        let err = PasswordFinder::default()
            .find(&mut harness.env, &who(&[]))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "read password: broken");
    }

    #[tokio::test]
    async fn no_password_anywhere_says_where_to_put_one() {
        let mut harness = Harness::new(&[], "");

        let err = PasswordFinder::default()
            .find(&mut harness.env, &who(&[]))
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "no password: run 'tamis login', or set account.password_command or $TAMIS_PASSWORD"
        );

        harness.env.prompt = Some(Box::new(|_| Ok(String::new())));
        assert!(
            PasswordFinder::default()
                .find(&mut harness.env, &who(&[]))
                .await
                .is_err(),
            "an empty answer is no password"
        );
    }
}

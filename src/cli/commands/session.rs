use std::io::Write;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};

use super::super::{App, password};
use crate::config::Config;
use crate::mail::client::{Client, DialConfig};
use crate::secret::{self, Keyring};

const KEYCHAIN_ALTERNATIVES: &str =
    "use account.password_command, or a systemd credential, instead";

/// Loads the configuration, provided there is a keychain to use.
fn keychain_config(app: &App) -> Result<(Config, Arc<dyn Keyring>)> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    match &app.env.keyring {
        Some(keyring) => Ok((cfg, Arc::clone(keyring))),
        None => bail!("no system keychain available; {KEYCHAIN_ALTERNATIVES}"),
    }
}

pub(in super::super) async fn login(app: &mut App) -> Result<()> {
    let (cfg, keyring) = keychain_config(app)?;
    let password = password::type_password(&mut app.env, &cfg.account.user)?;

    let dial = DialConfig {
        host: cfg.account.host.clone(),
        port: cfg.account.port,
        username: cfg.account.user.clone(),
        password: password.clone(),
        plaintext: app.env.plaintext_imap,
    };
    let client = Client::dial(&dial).await.context("check the password")?;
    client.close().await;

    keyring
        .set(
            &secret::service(&cfg.account.host),
            &cfg.account.user,
            &password,
        )
        .map_err(|err| anyhow::anyhow!("store the password: {err}; {KEYCHAIN_ALTERNATIVES}"))?;
    writeln!(
        app.env.stdout,
        "Saved the password for {} on {} in the system keychain.",
        cfg.account.user, cfg.account.host
    )?;
    Ok(())
}

pub(in super::super) fn logout(app: &mut App) -> Result<()> {
    let (cfg, keyring) = keychain_config(app)?;
    match keyring.delete(&secret::service(&cfg.account.host), &cfg.account.user) {
        Err(secret::Error::NotFound) => {
            writeln!(
                app.env.stdout,
                "No password stored for {} on {}.",
                cfg.account.user, cfg.account.host
            )?;
        }
        Err(err) => bail!("remove the password: {err}"),
        Ok(()) => writeln!(
            app.env.stdout,
            "Removed the password for {} on {} from the system keychain.",
            cfg.account.user, cfg.account.host
        )?,
    }
    Ok(())
}

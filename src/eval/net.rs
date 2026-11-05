//! The part of `tamis-eval fetch` that reaches the mail server.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};

use super::{Spec, fetch_folder, parse_spec};
use crate::config::{Config, default_path};
use crate::mail::client::{Client, DialConfig};
use crate::secret::{self, Keyring as _};

const PASSWORD_ENV: &str = "TAMIS_PASSWORD";

pub(super) async fn fetch_command(
    out: &mut impl Write,
    getenv: &(dyn Fn(&str) -> Option<String> + Sync),
    config: Option<PathBuf>,
    spam: &[String],
    ham: &[String],
    dir: &Path,
) -> Result<()> {
    let root = std::path::absolute(dir)?;
    check_outside_repo(&root)?;
    let specs = |texts: &[String]| {
        texts
            .iter()
            .map(|t| parse_spec(t).map_err(|e| anyhow!(e)))
            .collect::<Result<Vec<Spec>>>()
    };
    let (spam, ham) = (specs(spam)?, specs(ham)?);

    let path = config
        .or_else(|| default_path(getenv))
        .context("fetch: no configuration file")?;
    let cfg = Config::load(getenv, &path).context("fetch")?;
    let password = match getenv(PASSWORD_ENV).filter(|p| !p.is_empty()) {
        Some(password) => password,
        None => secret::System::new()
            .and_then(|keyring| keyring.get(&secret::service(&cfg.account.host), &cfg.account.user))
            .map_err(|err| anyhow!("fetch: {err} (run tamis login, or set ${PASSWORD_ENV})"))?,
    };
    let mut client = Client::dial(&DialConfig {
        host: cfg.account.host.clone(),
        port: cfg.account.port,
        username: cfg.account.user.clone(),
        password,
        plaintext: false,
    })
    .await
    .context("fetch")?;

    for (class, specs) in [("spam", spam), ("ham", ham)] {
        for spec in specs {
            let target = root.join(class).join(spec.folder.replace(['/', ' '], "_"));
            let written = fetch_folder(&mut client, &spec, &target).await?;
            writeln!(
                out,
                "{}: wrote {written} messages to {}",
                spec.folder,
                target.display()
            )?;
        }
    }
    client.close().await;
    writeln!(
        out,
        "\nThat is a copy of your mail. Delete {} when you are done.",
        root.display()
    )?;
    Ok(())
}

/// Refuses to put mail where it could be committed.
fn check_outside_repo(path: &Path) -> Result<()> {
    for dir in path.ancestors() {
        if [".git", ".jj"]
            .iter()
            .any(|marker| dir.join(marker).exists())
        {
            bail!(
                "refusing to copy mail into a repository: {} is inside the repository at {}; use a directory outside it",
                path.display(),
                dir.display()
            );
        }
    }
    Ok(())
}

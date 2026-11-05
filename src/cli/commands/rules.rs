use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

use super::super::App;
use super::common::connect;
use crate::config::Config;
use crate::mail::Mailbox;
use crate::rules::{self, Rules};

const FOLDER_CHECK_TIMEOUT: Duration = Duration::from_mins(1);

/// The file argument, or the configured script when there is none.
fn rules_path(app: &App, file: Option<&Path>) -> Result<PathBuf> {
    if let Some(file) = file {
        return Ok(file.to_path_buf());
    }
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    if cfg.rules.script.is_empty() {
        bail!("no rules script: set rules.script in the config file");
    }
    Ok(PathBuf::from(cfg.rules.script))
}

pub(in super::super) fn show(app: &mut App) -> Result<()> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    if cfg.rules.script.is_empty() {
        writeln!(app.env.stdout, "No rules script configured (rules.script).")?;
        return Ok(());
    }
    let source = std::fs::read_to_string(&cfg.rules.script).context("read rules")?;
    write!(app.env.stdout, "# {}\n{source}", cfg.rules.script)?;
    Ok(())
}

pub(in super::super) async fn check(
    app: &mut App,
    file: Option<&Path>,
    folders: bool,
) -> Result<()> {
    let path = rules_path(app, file)?;
    let loaded = Rules::load(&path).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    writeln!(app.env.stdout, "{}: ok", path.display())?;
    if !folders {
        return Ok(());
    }
    check_folders(app, loaded.targets()).await
}

/// Lists the mailbox and reports each target it lacks, with the nearest
/// existing name when one is close.
async fn check_folders(app: &mut App, targets: &[String]) -> Result<()> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    let existing = tokio::time::timeout(FOLDER_CHECK_TIMEOUT, async {
        let mut client = connect(app, &cfg).await?;
        let folders = client.folders().await.context("check folders");
        client.close().await;
        folders
    })
    .await
    .map_err(|_| anyhow!("check folders: timed out"))??;

    let absent = rules::missing(targets, &existing);
    for folder in &absent {
        match &folder.nearest {
            Some(nearest) => writeln!(
                app.env.stdout,
                "no such folder {:?} (did you mean {nearest:?}?)",
                folder.folder
            )?,
            None => writeln!(app.env.stdout, "no such folder {:?}", folder.folder)?,
        }
    }
    if !absent.is_empty() {
        bail!(
            "rules file into folders that don't exist: {} folder(s)",
            absent.len()
        );
    }
    writeln!(app.env.stdout, "all {} folders exist", targets.len())?;
    Ok(())
}

pub(in super::super) fn test(app: &mut App, message: &Path) -> Result<()> {
    let path = rules_path(app, None)?;
    let loaded = Rules::load(&path).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    let raw = std::fs::read(message).context("read message")?;

    let folder = loaded
        .folder(&raw, &rules::sender_of(&raw))
        .context("run rules")?;
    match folder {
        None => writeln!(app.env.stdout, "stays in the inbox")?,
        Some(folder) => writeln!(app.env.stdout, "filed into {folder}")?,
    }
    Ok(())
}

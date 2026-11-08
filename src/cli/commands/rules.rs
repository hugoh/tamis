use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

use super::super::App;
use super::common::connect;
use crate::config::Config;
use crate::mail::Mailbox;
use crate::rules::{self, Decision, Rules};

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
    strict: bool,
) -> Result<()> {
    let path = rules_path(app, file)?;
    let loaded = Rules::load(&path).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    writeln!(app.env.stdout, "{}: ok", path.display())?;
    for command in loaded.unsupported() {
        writeln!(
            app.env.stdout,
            "warning: the script uses {command}, which tamis ignores"
        )?;
    }
    if strict && !loaded.unsupported().is_empty() {
        bail!(
            "the script uses {} command(s) tamis ignores",
            loaded.unsupported().len()
        );
    }
    if !folders {
        return Ok(());
    }
    check_folders(app, loaded.targets()).await
}

pub(in super::super) fn extensions(app: &mut App, markdown: bool) -> Result<()> {
    for name in rules::supported_extensions() {
        if markdown {
            match rules::extension_reference(&name) {
                Some(url) => writeln!(app.env.stdout, "- [`{name}`]({url})")?,
                None => writeln!(app.env.stdout, "- `{name}`")?,
            }
        } else {
            writeln!(app.env.stdout, "{name}")?;
        }
    }
    Ok(())
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

/// What a message the rules file nowhere is expected to do.
const STAY: &str = "stay";

/// How `rules test` runs and reports.
pub(in super::super) struct RulesTest<'a> {
    pub script: Option<&'a Path>,
    pub expect_header: Option<&'a str>,
    pub strict: bool,
    pub json: bool,
}

type Verdict = Result<(), String>;

/// The header's expected folder and whether the decision met what was asked.
/// `None` means nothing was asked.
fn judge(
    options: &RulesTest<'_>,
    raw: &[u8],
    got: &str,
    decision: &Decision,
) -> (Option<String>, Option<Verdict>) {
    let mut verdict: Option<Verdict> = None;
    let mut expected = None;
    if let Some(header) = options.expect_header {
        expected = rules::header_value(raw, header);
        verdict = Some(match &expected {
            Some(want) if want == got => Ok(()),
            Some(want) => Err(format!("want {want:?}, got {got:?}")),
            None => Err(format!("no {header} header")),
        });
    }
    if options.strict && !decision.ignored.is_empty() && !matches!(verdict, Some(Err(_))) {
        verdict = Some(Err(format!("ignored: {}", decision.ignored.join("; "))));
    } else if options.strict && verdict.is_none() {
        verdict = Some(Ok(()));
    }
    (expected, verdict)
}

fn json_line(
    message: &Path,
    decision: &Decision,
    expected: Option<&str>,
    verdict: Option<&Verdict>,
) -> serde_json::Value {
    let mut object = serde_json::json!({
        "message": message.display().to_string(),
        "folder": decision.folder,
        "ignored": decision.ignored,
    });
    if let Some(want) = expected {
        object["expected"] = serde_json::json!(want);
    }
    if let Some(verdict) = verdict {
        object["ok"] = serde_json::json!(verdict.is_ok());
    }
    object
}

fn write_human(
    out: &mut dyn Write,
    message: &Path,
    alone: bool,
    decision: &Decision,
    verdict: Option<&Verdict>,
) -> Result<()> {
    let name = message.display();
    match verdict {
        Some(Ok(())) => writeln!(out, "ok {name}")?,
        Some(Err(why)) => writeln!(out, "FAIL {name}: {why}")?,
        None => {
            let result = match &decision.folder {
                None => "stays in the inbox".to_owned(),
                Some(folder) => format!("filed into {folder}"),
            };
            if alone {
                writeln!(out, "{result}")?;
            } else {
                writeln!(out, "{name}: {result}")?;
            }
        }
    }
    let already_said = matches!(verdict, Some(Err(why)) if why.starts_with("ignored:"));
    if !already_said {
        for ignored in &decision.ignored {
            writeln!(out, "  ignored: {ignored}")?;
        }
    }
    Ok(())
}

pub(in super::super) fn test(
    app: &mut App,
    messages: &[PathBuf],
    options: &RulesTest<'_>,
) -> Result<()> {
    let path = rules_path(app, options.script)?;
    let loaded = Rules::load(&path).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    let checking = options.expect_header.is_some() || options.strict;

    let mut failed = 0_usize;
    for message in messages {
        let raw = std::fs::read(message).with_context(|| format!("read {}", message.display()))?;
        let decision = loaded
            .decide(&raw, &rules::sender_of(&raw))
            .with_context(|| format!("run rules on {}", message.display()))?;
        let got = decision.folder.as_deref().unwrap_or(STAY);

        let (expected, verdict) = judge(options, &raw, got, &decision);
        if matches!(verdict, Some(Err(_))) {
            failed += 1;
        }

        if options.json {
            let line = json_line(message, &decision, expected.as_deref(), verdict.as_ref());
            writeln!(app.env.stdout, "{line}")?;
        } else {
            let alone = messages.len() == 1;
            write_human(
                &mut app.env.stdout,
                message,
                alone,
                &decision,
                verdict.as_ref(),
            )?;
        }
    }
    if checking {
        if !options.json {
            writeln!(
                app.env.stdout,
                "{} passed, {failed} failed",
                messages.len() - failed
            )?;
        }
        if failed > 0 {
            bail!(
                "{failed} of {} message(s) not filed as expected",
                messages.len()
            );
        }
    }
    Ok(())
}

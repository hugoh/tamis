//! `web enroll`, `web passkeys` and `web revoke`: who can sign in to the web
//! interface.

use std::io::Write;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;

use super::super::App;
use super::super::style::{BOLD, Cell, DIM, cell, write_table};
use super::common::{local, open, palette};
use crate::config::Config;
use crate::web::{hash_token, new_token};

const ENROLL_TTL: Duration = Duration::from_mins(15);

fn open_web(app: &App) -> Result<(Config, crate::store::Store)> {
    let (cfg, store) = open(app)?;
    if !cfg.web.enabled {
        bail!("web is not enabled: set enabled = true and origin in the [web] section");
    }
    Ok((cfg, store))
}

pub(in super::super) fn enroll(app: &mut App) -> Result<()> {
    let (cfg, store) = open_web(app)?;
    let token = new_token().map_err(|err| anyhow!("generate token: {err}"))?;
    store.add_enrollment(&hash_token(&token), Timestamp::now() + ENROLL_TTL)?;
    writeln!(
        app.env.stdout,
        "Open this link within {} minutes to enroll a passkey; it works once:\n{}/auth/enroll?token={token}",
        ENROLL_TTL.as_secs() / 60,
        cfg.web.origin.trim_end_matches('/'),
    )?;
    Ok(())
}

pub(in super::super) fn passkeys(app: &mut App) -> Result<()> {
    let (_, store) = open_web(app)?;
    let keys = store.passkeys()?;
    if keys.is_empty() {
        writeln!(
            app.env.stdout,
            "No passkeys enrolled. Run `tamis web enroll` to add one."
        )?;
        return Ok(());
    }
    let colors = palette(app)?;
    let mut rows: Vec<Vec<Cell>> = vec![vec![
        cell("ID", BOLD),
        cell("NAME", BOLD),
        cell("ADDED", BOLD),
        cell("LAST USED", BOLD),
    ]];
    for key in &keys {
        rows.push(vec![
            cell(&key.id.to_string(), ""),
            cell(&key.name, ""),
            cell(&local(key.created_at), DIM),
            cell(
                &key.last_used.map_or_else(|| "never".to_owned(), local),
                DIM,
            ),
        ]);
    }
    write_table(&mut app.env.stdout, colors, &rows)
}

pub(in super::super) fn revoke(app: &mut App, ids: &[i64]) -> Result<()> {
    let (_, store) = open_web(app)?;
    for &id in ids {
        store.remove_passkey(id)?;
        writeln!(app.env.stdout, "revoked passkey {id}")?;
    }
    Ok(())
}

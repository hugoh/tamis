//! `probe`: which IMAP features a server supports.

use std::net::IpAddr;

use anyhow::{Result, anyhow, bail};

use super::super::password::Who;
use super::super::{App, ProbeArgs, Reported};
use crate::config::{Config, parse_duration};
use crate::probe::{self, Config as ProbeConfig};

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub(in super::super) async fn probe(app: &mut App, args: &ProbeArgs) -> Result<()> {
    let idle_wait = parse_duration(&args.idle_wait).map_err(|err| anyhow!("--idle-wait: {err}"))?;

    let mut host = args.host.clone().unwrap_or_default();
    let mut user = args.user.clone().unwrap_or_default();
    let mut port = args.port;
    let mut command = Vec::new();
    if host.is_empty() || user.is_empty() {
        let cfg = Config::load(&*app.env.getenv, &app.config_path)
            .map_err(|err| anyhow!("probe: give --host and --user, or a configuration: {err}"))?;
        if host.is_empty() {
            host.clone_from(&cfg.account.host);
        }
        if user.is_empty() {
            user.clone_from(&cfg.account.user);
        }
        if port.is_none() {
            port = Some(cfg.account.port);
        }
        // The command only knows the account's own password.
        if host == cfg.account.host && user == cfg.account.user {
            command = cfg.account.password_command;
        }
    }

    let plaintext = args.plaintext || app.env.plaintext_imap;
    if args.plaintext && !is_loopback(&host) {
        bail!(
            "probe: --plaintext sends the password unencrypted, so it only connects to loopback hosts"
        );
    }

    let who = Who {
        host: &host,
        user: &user,
        command: &command,
    };
    let password = app.finder.find(&mut app.env, &who).await?;

    let report = probe::run(&ProbeConfig {
        host,
        port: port.unwrap_or(993),
        username: user,
        password,
        write: args.write,
        idle_wait,
        plaintext,
    })
    .await
    .map_err(|err| anyhow!("probe: {err}"))?;

    if args.json {
        crate::output::write_json(&mut app.env.stdout, &report)?;
    } else {
        report.write_text(&mut app.env.stdout)?;
    }
    if report.failed() {
        return Err(Reported.into());
    }
    Ok(())
}

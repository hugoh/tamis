use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::{ConfigInfo, Input, Report, RulesInfo, WINDOW, Web, load_daemon, run};
use crate::auth_pin::resolve_auth_pin;
use crate::config::{self, Config};
use crate::rules::Rules;
use crate::store::{self, Store};

const DIAL_TIMEOUT: Duration = Duration::from_secs(1);

/// Gathers what the checks look at from the store and runs them.
///
/// # Errors
/// Fails when the store can't be read.
pub fn collect(store: &Store, cfg: &Config) -> Result<Report, store::Error> {
    Ok(run(&Input {
        stats: store.stats()?,
        recent: store.decisions(WINDOW)?,
        retention: cfg.policy.journal_retention,
        auth: resolve_auth_pin(store, cfg)?,
        tokenizer_version: store.tokenizer_version()?,
        web: collect_web(store, &cfg.web)?,
        daemon: load_daemon(store, cfg.policy.poll_interval)?,
        config: config_info(cfg),
        rules: rules_info(&cfg.rules),
        now: Timestamp::now(),
        zone: TimeZone::system(),
    }))
}

/// Counts the passkeys and tries to connect to the listen address.
fn collect_web(store: &Store, cfg: &config::Web) -> Result<Web, store::Error> {
    let mut web = Web {
        enabled: cfg.enabled,
        listen: cfg.listen.clone(),
        origin: cfg.origin.clone(),
        ..Web::default()
    };
    if !cfg.enabled {
        return Ok(web);
    }

    web.passkeys = store.passkeys()?.len();
    web.reachable = cfg.listen.to_socket_addrs().is_ok_and(|mut addresses| {
        addresses.any(|address| TcpStream::connect_timeout(&address, DIAL_TIMEOUT).is_ok())
    });
    Ok(web)
}

fn config_info(cfg: &Config) -> ConfigInfo {
    ConfigInfo {
        path: cfg.path.display().to_string(),
        account: format!("{} on {}", cfg.account.user, cfg.account.host),
        mode: if cfg.policy.mode == config::Mode::Live {
            "live"
        } else {
            "shadow"
        }
        .to_owned(),
        watch: cfg.folders.watch.join(" "),
        train: format!("{}, {}", cfg.folders.train_spam, cfg.folders.train_good),
        state: cfg.state.display().to_string(),
    }
}

/// Compiles the script again, so a file edited or removed since the
/// configuration loaded shows up.
fn rules_info(cfg: &config::Rules) -> RulesInfo {
    if cfg.script.is_empty() {
        return RulesInfo::default();
    }
    match Rules::load(Path::new(&cfg.script)) {
        Ok(rules) => RulesInfo {
            script: cfg.script.clone(),
            targets: rules.targets().to_vec(),
            error: None,
        },
        Err(err) => RulesInfo {
            script: cfg.script.clone(),
            targets: Vec::new(),
            error: Some(err.to_string()),
        },
    }
}

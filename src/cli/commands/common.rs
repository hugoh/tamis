use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::super::App;
use super::super::password::Who;
use crate::config::Config;
use crate::engine::Engine;
use crate::mail::client::{Client, DialConfig};
use crate::output::style::Palette;
use crate::rules::Rules;
use crate::store::Store;

/// Loads the configuration and opens the state database.
pub(super) fn open(app: &App) -> Result<(Config, Store)> {
    let cfg = Config::load(&*app.env.getenv, &app.config_path)?;
    if let Some(dir) = cfg.state.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).context("create state directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Best effort: the directory may belong to someone else's setup.
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
    }
    let store = Store::open(&cfg.state).context("open state")?;
    Ok((cfg, store))
}

pub(super) fn palette(app: &App) -> Result<Palette> {
    Palette::new(&app.color, app.env.stdout_is_terminal, &*app.env.getenv)
}

/// A time the way a local CLI shows it.
pub(super) fn local(at: Timestamp) -> String {
    at.to_zoned(TimeZone::system())
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Connects to the mailbox, finding the password where it is kept.
pub(super) async fn connect(app: &mut App, cfg: &Config) -> Result<Client> {
    let who = Who {
        host: &cfg.account.host,
        user: &cfg.account.user,
        command: &cfg.account.password_command,
    };
    let password = app.finder.find(&mut app.env, &who).await?;
    let dial = DialConfig {
        host: cfg.account.host.clone(),
        port: cfg.account.port,
        username: cfg.account.user.clone(),
        password,
        plaintext: app.env.plaintext_imap,
    };
    Client::dial(&dial).await.context("connect")
}

/// An engine on a fresh connection, with the filing rules if any.
pub(super) async fn engine_for(
    app: &mut App,
    cfg: &Arc<Config>,
    store: &Arc<Store>,
) -> Result<Engine<Client>> {
    let client = connect(app, cfg).await?;
    let mut engine = Engine::new(client, Arc::clone(store), Arc::clone(cfg));
    if !cfg.rules.script.is_empty() {
        engine.set_rules(Rules::load(Path::new(&cfg.rules.script))?);
    }
    Ok(engine)
}

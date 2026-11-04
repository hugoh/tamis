//! `tamis run`: judge new mail, then keep watching.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use jiff::Timestamp;
use tokio::sync::Notify;

use super::super::password::{PasswordFinder, Who};
use super::super::{App, Env};
use super::common::open;
use crate::config::Config;
use crate::daemon::{Connector, Daemon, Reload, WATCH_RETRY};
use crate::live::{SETTING_KEY, Status};
use crate::mail;
use crate::mail::client::{Client, DialConfig};
use crate::mail::watch::Watcher;
use crate::monitor::Pinger;
use crate::store::Store;

/// Reaches the real mailbox, finding the password afresh where it is kept.
struct ImapConnector {
    env: Env,
    finder: PasswordFinder,
}

impl ImapConnector {
    async fn dial_config(&mut self, cfg: &Config) -> mail::Result<DialConfig> {
        let who = Who {
            host: &cfg.account.host,
            user: &cfg.account.user,
            command: &cfg.account.password_command,
        };
        let password = self
            .finder
            .find(&mut self.env, &who)
            .await
            .map_err(|err| mail::Error::Server(format!("{err:#}")))?;
        Ok(DialConfig {
            host: cfg.account.host.clone(),
            port: cfg.account.port,
            username: cfg.account.user.clone(),
            password,
            plaintext: self.env.plaintext_imap,
        })
    }
}

impl Connector for ImapConnector {
    type Mailbox = Client;

    async fn connect(&mut self, cfg: &Config) -> mail::Result<Client> {
        let dial = self.dial_config(cfg).await?;
        Client::dial(&dial).await
    }

    /// Watches the watched folders. It watches nothing when the password
    /// can't be found; polling still covers them.
    async fn watch(&mut self, cfg: &Config) -> Option<Watcher> {
        match self.dial_config(cfg).await {
            Ok(dial) => Some(Watcher::start(&dial, &cfg.folders.watch, WATCH_RETRY)),
            Err(err) => {
                tracing::warn!(%err, "not watching for new mail; polling");
                None
            }
        }
    }
}

pub(in super::super) async fn run(app: &mut App, once: bool) -> Result<()> {
    let (cfg, store) = open(app)?;
    let (cfg, store) = (Arc::new(cfg), Arc::new(store));
    tracing::info!(version = %app.env.version, "tamis starting");

    let status = Arc::new(Status::new(Timestamp::now()));
    let saved = Arc::clone(&store);
    status.persist(move |snapshot| save_status(&saved, snapshot));
    status.save(Timestamp::now());

    let getenv = Arc::clone(&app.env.getenv);
    let path = app.config_path.clone();
    let reload: Reload = Box::new(move || Config::load(&*getenv, &path));

    let hangup = Arc::new(Notify::new());
    #[cfg(unix)]
    spawn_hangup_listener(Arc::clone(&hangup));

    let connector = ImapConnector {
        env: app.env.take(),
        finder: std::mem::take(&mut app.finder),
    };
    let beat =
        Pinger::new(&cfg.monitor.ping_url).map_err(|err| anyhow!("monitor.ping_url: {err}"))?;
    let quality = Pinger::new(&cfg.monitor.quality_url)
        .map_err(|err| anyhow!("monitor.quality_url: {err}"))?;

    let mut daemon = Daemon::new(connector, store, cfg, reload, app.shutdown.clone(), hangup)
        .with_status(status)
        .with_heartbeat(beat)
        .with_quality(quality);

    if once {
        daemon.once().await?;
    } else {
        daemon.run().await;
    }
    Ok(())
}

fn save_status(store: &Store, snapshot: &crate::live::Snapshot) {
    if let Err(err) = store.set_setting(SETTING_KEY, &snapshot.encode()) {
        tracing::debug!(%err, "could not save the daemon status");
    }
}

/// Has SIGHUP wake the daemon to reload its configuration.
#[cfg(unix)]
fn spawn_hangup_listener(hangup: Arc<Notify>) {
    use tokio::signal::unix::{SignalKind, signal};

    let Ok(mut stream) = signal(SignalKind::hangup()) else {
        return;
    };
    tokio::spawn(async move {
        while stream.recv().await.is_some() {
            hangup.notify_one();
        }
    });
}

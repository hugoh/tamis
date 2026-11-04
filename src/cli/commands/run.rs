//! `tamis run`: judge new mail, then keep watching.

use std::sync::Arc;

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::super::password::{PasswordFinder, Who};
use super::super::{App, Env};
use super::common::open;
use crate::config::Config;
use crate::daemon::{Connector, Daemon, PASS_TIMEOUT, Reload, WATCH_RETRY};
use crate::engine::Engine;
use crate::live::{SETTING_KEY, Status};
use crate::mail;
use crate::mail::client::{Client, DialConfig};
use crate::mail::watch::Watcher;
use crate::monitor::Pinger;
use crate::rules::Rules;
use crate::store::Store;
use crate::web::{Job, Options, RunJobs, Server};

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

/// Lets the daemon and the web interface's jobs share one way to reach the
/// mailbox, and one remembered password.
struct SharedConnector(Arc<tokio::sync::Mutex<ImapConnector>>);

impl Connector for SharedConnector {
    type Mailbox = Client;

    async fn connect(&mut self, cfg: &Config) -> mail::Result<Client> {
        self.0.lock().await.connect(cfg).await
    }

    async fn watch(&mut self, cfg: &Config) -> Option<Watcher> {
        self.0.lock().await.watch(cfg).await
    }
}

/// Runs what a signed-in user asks of a decision on a fresh connection.
fn job_runner(
    connector: Arc<tokio::sync::Mutex<ImapConnector>>,
    cfg: Arc<Config>,
    store: Arc<Store>,
) -> RunJobs {
    Arc::new(move |job| {
        let (connector, cfg, store) =
            (Arc::clone(&connector), Arc::clone(&cfg), Arc::clone(&store));
        Box::pin(async move {
            let dial = connector
                .lock()
                .await
                .dial_config(&cfg)
                .await
                .map_err(|err| err.to_string())?;
            let client = Client::dial(&dial)
                .await
                .map_err(|err| format!("connect: {err}"))?;
            let mut engine = Engine::new(client, store, Arc::clone(&cfg));
            if !cfg.rules.script.is_empty() {
                let rules =
                    Rules::load(Path::new(&cfg.rules.script)).map_err(|err| err.to_string())?;
                engine.set_rules(rules);
            }
            let outcome = tokio::time::timeout(PASS_TIMEOUT, async {
                match job {
                    Job::Undo(id) => engine.undo(id).await,
                    Job::Learn(id, label) => engine.learn(id, label).await,
                }
            })
            .await;
            engine.into_mailbox().close().await;
            match outcome {
                Ok(done) => done.map_err(|err| err.to_string()),
                Err(_) => Err("timed out".to_owned()),
            }
        })
    })
}

/// Serves the web interface until `shutdown` is cancelled, if it is enabled.
async fn start_web(
    cfg: &Arc<Config>,
    store: &Arc<Store>,
    options: Options,
    shutdown: CancellationToken,
) -> Result<Option<tokio::task::JoinHandle<()>>> {
    if !cfg.web.enabled {
        return Ok(None);
    }
    if cfg.web.tls() {
        bail!(
            "web interface: serving HTTPS directly is not supported yet; use a TLS-terminating proxy with an https origin"
        );
    }
    let server = Server::new(Arc::clone(cfg), Arc::clone(store), options)
        .map_err(|err| anyhow!("web interface: {err}"))?;
    let listener = tokio::net::TcpListener::bind(&cfg.web.listen)
        .await
        .map_err(|err| anyhow!("web interface: listen on {}: {err}", cfg.web.listen))?;
    tracing::info!(addr = %cfg.web.listen, origin = %cfg.web.origin, "web interface up");
    Ok(Some(tokio::spawn(async move {
        if let Err(err) = server.serve(listener, shutdown).await {
            tracing::error!(%err, "web interface stopped");
        }
    })))
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

    let connector = Arc::new(tokio::sync::Mutex::new(ImapConnector {
        env: app.env.take_credentials(),
        finder: std::mem::take(&mut app.finder),
    }));
    let beat =
        Pinger::new(&cfg.monitor.ping_url).map_err(|err| anyhow!("monitor.ping_url: {err}"))?;
    let quality = Pinger::new(&cfg.monitor.quality_url)
        .map_err(|err| anyhow!("monitor.quality_url: {err}"))?;

    let web = if once {
        None
    } else {
        let options = Options {
            run: Some(job_runner(
                Arc::clone(&connector),
                Arc::clone(&cfg),
                Arc::clone(&store),
            )),
            status: Some(Arc::clone(&status)),
            version: app.env.version.clone(),
        };
        start_web(&cfg, &store, options, app.shutdown.clone()).await?
    };

    let mut daemon = Daemon::new(
        SharedConnector(connector),
        store,
        cfg,
        reload,
        app.shutdown.clone(),
        hangup,
    )
    .with_status(status)
    .with_heartbeat(beat)
    .with_quality(quality);

    if once {
        daemon.once().await?;
    } else {
        daemon.run().await;
    }
    if let Some(web) = web {
        let _ = web.await;
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

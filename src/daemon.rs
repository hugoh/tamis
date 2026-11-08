//! `tamis run`: passes over the account, repeated when a watcher hears of new
//! mail or flag changes and otherwise every `poll_interval`, with the retries,
//! reloads and housekeeping a long-running process needs.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::config::{self, Config};
use crate::engine::{self, Engine};
use crate::health;
use crate::live::{Status, Waiting};
use crate::mail::watch::Watcher;
use crate::mail::{self, Mailbox};
use crate::monitor::{self, Pinger};
use crate::retry::{self, Backoff, Streak, log_at};
use crate::rules::Rules;
use crate::store::Store;

/// A pass lasts at most this long, and when it ends the connection is
/// dropped, so a server that stops answering can't stall the loop.
pub const PASS_TIMEOUT: Duration = Duration::from_mins(10);
/// How long a pass in progress gets to finish once shutdown is asked for,
/// before it is dropped. Keep systemd's `TimeoutStopSec` above this.
const STOP_GRACE: Duration = Duration::from_secs(30);
/// How long to wait after the watcher hears of news, so a burst of mail is
/// handled by one pass.
const NEW_MAIL_SETTLE: Duration = Duration::from_secs(1);
/// Caps the wait after failed passes.
const MAX_PASS_RETRY: Duration = Duration::from_mins(15);
/// The wait after a rejected login: long, so a wrong password doesn't lock the
/// account, yet a provider's passing trouble heals without a restart.
const LOGIN_RETRY: Duration = Duration::from_mins(15);
/// How soon the watcher retries a folder whose connection failed.
pub const WATCH_RETRY: Duration = Duration::from_secs(5);
/// The journal is pruned at most this often.
const PRUNE_EVERY: Duration = Duration::from_hours(24);

/// How the daemon reaches the mailbox.
#[trait_variant::make(Connector: Send)]
pub trait LocalConnector {
    type Mailbox: Mailbox;

    /// Opens a fresh connection for one pass.
    async fn connect(&mut self, config: &Config) -> mail::Result<Self::Mailbox>;

    /// Starts watching the watched folders, or `None` when that isn't possible
    /// (polling covers them).
    async fn watch(&mut self, config: &Config) -> Option<Watcher>;
}

fn summary_text(summary: &engine::Summary) -> String {
    format!(
        "scanned {}, spam {}, uncertain {}, good {}, corrected {}, trained {}, rescued {}",
        summary.scanned,
        summary.spam,
        summary.uncertain,
        summary.good,
        summary.corrected,
        summary.trained,
        summary.rescued
    )
}

/// Reads the configuration again, on SIGHUP.
pub type Reload = Box<dyn FnMut() -> Result<Config, config::Error> + Send>;

pub struct Daemon<C: Connector> {
    connector: C,
    store: Arc<Store>,
    cfg: Arc<Config>,
    reload: Reload,
    shutdown: CancellationToken,
    hangup: Arc<Notify>,

    pace: Backoff,
    streak: Streak,
    /// The streak began with a rejected login.
    login: bool,
    check_rules: bool,
    last_prune: Option<tokio::time::Instant>,
    watcher: Option<Watcher>,
    status: Arc<Status>,
    /// The heartbeat check; inert when monitoring isn't configured.
    beat: Pinger,
    /// The daily quality check; inert when monitoring isn't configured.
    quality: Pinger,
}

/// What ended the wait between passes.
enum Wake {
    Next,
    Reload,
    Stop,
}

impl<C: Connector> Daemon<C> {
    pub fn new(
        connector: C,
        store: Arc<Store>,
        cfg: Arc<Config>,
        reload: Reload,
        shutdown: CancellationToken,
        hangup: Arc<Notify>,
    ) -> Self {
        let pace = Backoff::new(cfg.policy.poll_interval, MAX_PASS_RETRY);
        let check_rules = !cfg.rules.script.is_empty();
        Self {
            connector,
            store,
            cfg,
            reload,
            shutdown,
            hangup,
            pace,
            streak: Streak::default(),
            login: false,
            check_rules,
            last_prune: None,
            watcher: None,
            status: Arc::new(Status::new(Timestamp::now())),
            beat: Pinger::disabled(),
            quality: Pinger::disabled(),
        }
    }

    /// Reports passes and failures to `status`, for the web interface and for
    /// the commands that run in another process.
    #[must_use]
    pub fn with_status(mut self, status: Arc<Status>) -> Self {
        self.status = status;
        self
    }

    /// Pings `quality` once a day with whether the training and the decisions
    /// look healthy.
    #[must_use]
    pub fn with_quality(mut self, quality: Pinger) -> Self {
        self.quality = quality;
        self
    }

    /// Pings `beat` after passes that work and when trouble goes on.
    #[must_use]
    pub fn with_heartbeat(mut self, beat: Pinger) -> Self {
        self.beat = beat;
        self
    }

    /// Makes a single pass and returns what it did. A pass cut short by a
    /// requested shutdown is not a failure: it returns an empty summary and
    /// the unhandled mail waits for the next pass.
    ///
    /// # Errors
    /// Whatever stopped the pass.
    pub async fn once(&mut self) -> engine::Result<engine::Summary> {
        match self.pass().await {
            Err(err) if err.is_stopped() => Ok(engine::Summary::default()),
            other => other,
        }
    }

    /// Runs passes until shut down.
    pub async fn run(&mut self) {
        loop {
            self.prune_journal();

            self.status.wait(Waiting::Running);
            let outcome = self.pass().await;
            if self.shutdown.is_cancelled() {
                break;
            }

            let failure_wait = self.after(&outcome);
            self.check_quality();
            let wake = match outcome {
                Err(err) => {
                    let now = Timestamp::now();
                    self.status.failed(now, &err.to_string());
                    if err.is_login_rejected() {
                        self.status.login_rejected();
                    }
                    self.status
                        .retry_at(now.checked_add(failure_wait).unwrap_or(now));
                    self.status.wait(Waiting::Polling);
                    self.sleep(failure_wait).await
                }
                Ok(_) => self.wait_for_next_pass().await,
            };
            match wake {
                Wake::Stop => break,
                Wake::Reload => self.reload().await,
                Wake::Next => {}
            }
        }

        if let Some(watcher) = self.watcher.take() {
            watcher.close().await;
        }
        let beat = std::mem::replace(&mut self.beat, Pinger::disabled());
        beat.close().await;
        let quality = std::mem::replace(&mut self.quality, Pinger::disabled());
        quality.close().await;
    }

    /// One pass over the account on a fresh connection.
    async fn pass(&mut self) -> engine::Result<engine::Summary> {
        let cfg = Arc::clone(&self.cfg);
        let mailbox = self
            .connector
            .connect(&cfg)
            .await
            .map_err(|err| engine::Error::Context {
                context: "connect".to_owned(),
                source: Box::new(err.into()),
            })?;

        let mut engine = Engine::new(mailbox, Arc::clone(&self.store), Arc::clone(&cfg));
        engine.set_shutdown(self.shutdown.clone());
        if !cfg.rules.script.is_empty() {
            engine.set_rules(Rules::load(std::path::Path::new(&cfg.rules.script))?);
        }

        let check_rules = std::mem::take(&mut self.check_rules);
        let work = async {
            if check_rules && let Err(err) = engine.warn_missing_rule_folders().await {
                tracing::warn!(%err, "could not check the rule folders");
            }
            engine.run_once().await
        };

        let stop = self.shutdown.clone();
        let grace = async {
            stop.cancelled().await;
            tokio::time::sleep(STOP_GRACE).await;
        };
        let summary = tokio::select! {
            outcome = tokio::time::timeout(PASS_TIMEOUT, work) => match outcome {
                Ok(result) => result?,
                Err(_) => {
                    return Err(mail::Error::Connection("the pass took too long".to_owned()).into());
                }
            },
            () = grace => return Err(engine::Error::Stopped),
        };

        self.status
            .pass_done(Timestamp::now(), &summary_text(&summary));
        tracing::debug!(
            mode = ?cfg.policy.mode,
            scanned = summary.scanned,
            spam = summary.spam,
            uncertain = summary.uncertain,
            good = summary.good,
            corrected = summary.corrected,
            trained = summary.trained,
            rescued = summary.rescued,
            "pass complete"
        );
        Ok(summary)
    }

    /// Takes the outcome of a pass and returns how long to wait before trying
    /// again after a failure. Logs the trouble when it starts, now and then
    /// while it lasts, and when it ends.
    fn after(&mut self, outcome: &engine::Result<engine::Summary>) -> Duration {
        let Err(err) = outcome else {
            self.recovered();
            return Duration::ZERO;
        };

        let failures = self.streak.fail(std::time::SystemTime::now());
        self.tell(err, failures);

        if err.is_login_rejected() {
            self.login = self.login || failures == 1;
            let message = if failures == 1 {
                "login rejected; check the password"
            } else {
                "login still rejected"
            };
            log_at!(
                retry::rejected_level(failures),
                %err, failures, retry = ?LOGIN_RETRY, hint = "SIGHUP tries again at once", "{message}"
            );
            return LOGIN_RETRY;
        }

        let wait = self.pace.next_delay();
        let message = if failures == 1 {
            "pass failed"
        } else {
            "pass still failing"
        };
        log_at!(retry::level(failures, tracing::Level::ERROR), %err, failures, retry = ?wait, "{message}");
        wait
    }

    /// Pings the quality check when it is due: a failure listing what looks
    /// wrong with the training or the decisions, or a success. The schedule
    /// lives in the state database, so a restart doesn't move it.
    fn check_quality(&self) {
        if !self.quality.is_enabled() {
            return;
        }
        match monitor::take_quality_slot(&self.store, Timestamp::now()) {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                tracing::debug!(%err, "could not read the quality schedule");
                return;
            }
        }

        match health::collect(&self.store, &self.cfg) {
            Ok(report) => match report.quality_body() {
                Some(body) => self.quality.fail(&body),
                None => self
                    .quality
                    .success("the training and the decisions look healthy"),
            },
            Err(err) => tracing::debug!(%err, "could not check the filter's quality"),
        }
    }

    /// Reports a failed pass to the monitoring service: a rejected login at
    /// once, anything else once it has gone on for `fail_after` passes. Fewer
    /// are left to the check's grace time, so a short outage doesn't page
    /// anyone.
    fn tell(&self, err: &engine::Error, failures: u32) {
        if err.is_login_rejected() {
            self.beat.fail(&format!("login rejected: {err}"));
        } else if failures >= self.cfg.monitor.fail_after {
            let since = self.streak.since().map_or_else(String::new, |at| {
                Timestamp::try_from(at)
                    .map(|t| t.to_string())
                    .unwrap_or_default()
            });
            self.beat.fail(&format!(
                "{failures} passes failed in a row since {since}: {err}"
            ));
        }
    }

    /// Ends the trouble after a pass that worked, saying so if there was.
    fn recovered(&mut self) {
        self.pace.reset();
        self.beat.success(&self.status.snapshot().last_summary);
        if let Some((failures, downtime)) = self.streak.recover(std::time::SystemTime::now()) {
            let message = if self.login {
                "login accepted again"
            } else {
                "pass recovered"
            };
            tracing::info!(failures, ?downtime, "{message}");
        }
        self.login = false;
    }

    /// Sleeps, unless told to stop or to reload.
    async fn sleep(&mut self, wait: Duration) -> Wake {
        tokio::select! {
            () = self.shutdown.cancelled() => Wake::Stop,
            () = self.hangup.notified() => Wake::Reload,
            () = tokio::time::sleep(wait) => Wake::Next,
        }
    }

    /// Waits until the watcher hears of new mail or flag changes, or
    /// `poll_interval` passes: the poll covers the folders that aren't watched
    /// and a watcher that has dropped out. The watcher starts after the first
    /// good pass, so a rejected login is tried once, not once per folder.
    async fn wait_for_next_pass(&mut self) -> Wake {
        if self.watcher.is_none() {
            let cfg = Arc::clone(&self.cfg);
            self.watcher = self.connector.watch(&cfg).await;
            tracing::info!(folders = ?cfg.folders.watch, "imap loop started");
        }
        let poll = self.cfg.policy.poll_interval;
        let idling = self.watcher.as_ref().is_some_and(|w| w.idling() > 0);
        self.status.wait(if idling {
            Waiting::Idle
        } else {
            Waiting::Polling
        });

        let news = async {
            match &self.watcher {
                Some(watcher) => watcher.changed().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = self.shutdown.cancelled() => Wake::Stop,
            () = self.hangup.notified() => Wake::Reload,
            () = news => self.sleep(NEW_MAIL_SETTLE).await,
            () = tokio::time::sleep(poll) => Wake::Next,
        }
    }

    /// Swaps in the configuration file's current contents, or keeps the old
    /// one when it no longer loads. Rules are read afresh on every pass. The
    /// state database keeps its startup settings.
    async fn reload(&mut self) {
        match (self.reload)() {
            Err(err) => tracing::error!(%err, "SIGHUP: keeping the current configuration"),
            Ok(fresh) => {
                tracing::info!("SIGHUP: configuration and rules reloaded");
                if let Some(watcher) = self.watcher.take() {
                    watcher.close().await;
                }
                self.check_rules = !fresh.rules.script.is_empty();
                self.pace = Backoff::new(fresh.policy.poll_interval, MAX_PASS_RETRY);
                self.cfg = Arc::new(fresh);
            }
        }
    }

    /// Deletes decisions older than `journal_retention`, at most once a day.
    fn prune_journal(&mut self) {
        let retention = self.cfg.policy.journal_retention;
        if retention.is_zero() {
            return;
        }

        let now = tokio::time::Instant::now();
        if let Some(last) = self.last_prune
            && now.duration_since(last) < PRUNE_EVERY
        {
            tracing::debug!("journal prune skipped");
            return;
        }

        let Ok(cutoff) = Timestamp::now().checked_sub(retention) else {
            return;
        };
        match self.store.prune_decisions(cutoff) {
            Ok(pruned) => {
                self.last_prune = Some(now);
                tracing::debug!(?retention, %cutoff, decisions = pruned, "journal prune");
            }
            Err(err) => tracing::error!(%err, "journal pruning failed"),
        }
    }
}

#[cfg(test)]
mod tests;

//! IDLE on one long-lived connection per folder, reporting when mail arrives
//! or flags change. Each connection stays up for as long as the server allows,
//! so a quiet mailbox costs no logins.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::client::{Client, DialConfig};
use super::{Error, Result};
use crate::retry::{self, Backoff, Streak, log_at};

/// Caps the wait between reconnection attempts.
const MAX_WATCH_RETRY: Duration = Duration::from_mins(10);
/// The wait after a rejected login: long, so a wrong password isn't tried
/// often enough to lock the account, yet a provider's passing trouble still
/// heals without a restart.
const LOGIN_RETRY: Duration = Duration::from_mins(15);
/// How long a connection must have stayed up for the next failure to be
/// retried quickly again.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// IDLE is ended and restarted this often, inside the 29-minute limit of
/// RFC 2177.
const RENEW_IDLE: Duration = Duration::from_mins(28);

#[derive(Debug, Default)]
struct Shared {
    changed: Notify,
    idling: AtomicUsize,
}

/// Fires a [`Watcher`] by hand, for tests.
#[derive(Clone, Debug)]
pub struct Trigger(Arc<Shared>);

impl Trigger {
    pub fn fire(&self) {
        self.0.changed.notify_one();
    }
}

/// Watches folders until closed. A folder whose connection fails is retried
/// after about the first retry time, then ever more slowly while it keeps
/// failing; one whose login is rejected is retried rarely; one that can't IDLE
/// is given up on, since retrying won't help. Callers that also poll lose
/// nothing by a folder dropping out.
#[derive(Debug)]
pub struct Watcher {
    shared: Arc<Shared>,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching `mailboxes`.
    #[must_use]
    pub fn start(config: &DialConfig, mailboxes: &[String], first_retry: Duration) -> Self {
        let shared = Arc::new(Shared::default());
        let cancel = CancellationToken::new();

        let tasks = mailboxes
            .iter()
            .map(|mailbox| {
                let keeper = Keeper {
                    config: config.clone(),
                    mailbox: mailbox.clone(),
                    shared: Arc::clone(&shared),
                    cancel: cancel.clone(),
                    pace: Backoff::new(first_retry, MAX_WATCH_RETRY),
                    streak: Streak::default(),
                };
                tokio::spawn(keeper.keep())
            })
            .collect();
        Self {
            shared,
            cancel,
            tasks,
        }
    }

    /// A watcher that watches nothing, and the trigger that makes it report a
    /// change.
    #[must_use]
    pub fn detached() -> (Self, Trigger) {
        let shared = Arc::new(Shared::default());
        let watcher = Self {
            shared: Arc::clone(&shared),
            cancel: CancellationToken::new(),
            tasks: Vec::new(),
        };
        (watcher, Trigger(shared))
    }

    /// Resolves when a watched folder changes. Changes that pile up before
    /// anyone looks are reported once.
    pub async fn changed(&self) {
        self.shared.changed.notified().await;
    }

    /// How many folders are waiting in IDLE right now.
    #[must_use]
    pub fn idling(&self) -> usize {
        self.shared.idling.load(Ordering::Relaxed)
    }

    /// Stops watching and waits for the connections to end.
    pub async fn close(self) {
        self.cancel.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

struct Keeper {
    config: DialConfig,
    mailbox: String,
    shared: Arc<Shared>,
    cancel: CancellationToken,
    pace: Backoff,
    streak: Streak,
}

impl Keeper {
    async fn keep(mut self) {
        loop {
            let started = Instant::now();
            let err = match self.idle_on().await {
                Ok(()) => return,
                Err(err) => err,
            };
            if self.cancel.is_cancelled() {
                return;
            }
            if err == Error::NoIdle {
                tracing::warn!(mailbox = %self.mailbox, "server has no IDLE; polling only");
                return;
            }

            let wait = self.begin(&err, started.elapsed());
            tokio::select! {
                () = self.cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Takes the error that ended a connection that stayed up for the given
    /// time, logs it as it deserves and returns how long to wait before
    /// dialling.
    fn begin(&mut self, err: &Error, stayed_up: Duration) -> Duration {
        let failures = self.streak.fail(SystemTime::now());
        let mailbox = self.mailbox.as_str();

        if matches!(err, Error::Login(_)) {
            let message = if failures == 1 {
                "login rejected; trying again later"
            } else {
                "login still rejected"
            };
            log_at!(retry::rejected_level(failures), mailbox, %err, failures, retry = ?LOGIN_RETRY, "{message}");
            return LOGIN_RETRY;
        }

        if stayed_up >= STABLE_AFTER {
            self.pace.reset();
        }
        let wait = self.pace.next_delay();
        let message = if failures == 1 {
            "watch failed; retrying"
        } else {
            "watch still failing"
        };
        log_at!(retry::level(failures, tracing::Level::WARN), mailbox, %err, failures, retry = ?wait, "{message}");
        wait
    }

    /// Logs the end of trouble, if there was any.
    fn over(&mut self) {
        if let Some((failures, downtime)) = self.streak.recover(SystemTime::now()) {
            tracing::info!(mailbox = %self.mailbox, failures, ?downtime, "imap loop recovered");
        }
    }

    /// IDLEs on the folder, reporting changes, until the connection fails
    /// (`Err`) or the watcher is closed (`Ok`).
    async fn idle_on(&mut self) -> Result<()> {
        let mut client = Client::dial(&self.config).await?;
        if !client.supports_idle() {
            return Err(Error::NoIdle);
        }

        let _idling = IdleGuard::new(Arc::clone(&self.shared));
        self.over();
        tracing::debug!(mailbox = %self.mailbox, "idle");

        loop {
            let outcome = tokio::select! {
                () = self.cancel.cancelled() => return Ok(()),
                outcome = client.idle(&self.mailbox, RENEW_IDLE) => outcome,
            };
            if outcome? {
                tracing::debug!(mailbox = %self.mailbox, "idle ended: the server reported a change");
                self.shared.changed.notify_one();
            } else {
                tracing::debug!(mailbox = %self.mailbox, "idle renewed");
            }
        }
    }
}

/// Counts a folder as idling for as long as it lives.
struct IdleGuard(Arc<Shared>);

impl IdleGuard {
    fn new(shared: Arc<Shared>) -> Self {
        shared.idling.fetch_add(1, Ordering::Relaxed);
        Self(shared)
    }
}

impl Drop for IdleGuard {
    fn drop(&mut self) {
        self.0.idling.fetch_sub(1, Ordering::Relaxed);
    }
}

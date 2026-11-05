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

    /// Makes a single pass and returns what it did.
    ///
    /// # Errors
    /// Whatever stopped the pass.
    pub async fn once(&mut self) -> engine::Result<engine::Summary> {
        self.pass().await
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

        let summary = match tokio::time::timeout(PASS_TIMEOUT, work).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(mail::Error::Connection("the pass took too long".to_owned()).into());
            }
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
#[allow(
    clippy::unused_async_trait_impl,
    reason = "the connector trait is async; the test double has nothing to await"
)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jiff::Timestamp;
    use tokio::sync::Mutex;

    use super::*;
    use crate::config::{Account, Mode};
    use crate::mail::fake::Fake;
    use crate::mail::watch::Trigger;
    use crate::mail::{Message, ModState, State};
    use crate::store::Decision;

    /// A handle on one fake mailbox that every connection shares, as every
    /// connection to a real server shares its mail.
    #[derive(Clone)]
    struct Shared(Arc<Mutex<Fake>>);

    macro_rules! delegate {
        ($(fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
            $(async fn $name(&mut self $(, $arg: $ty)*) -> mail::Result<$ret> {
                self.0.lock().await.$name($($arg),*).await
            })*
        };
    }

    impl Mailbox for Shared {
        delegate! {
            fn state(&mut self, mailbox: &str) -> State;
            fn messages(&mut self, mailbox: &str) -> u32;
            fn uids_after(&mut self, mailbox: &str, after: u32) -> Vec<u32>;
            fn seen_before(&mut self, mailbox: &str, before: Timestamp) -> Vec<u32>;
            fn find_message_id(&mut self, mailbox: &str, message_id: &str) -> Vec<u32>;
            fn mod_state(&mut self, mailbox: &str) -> Option<ModState>;
            fn search_keywords(&mut self, mailbox: &str, keywords: &[String], since: Timestamp, changed_since: u64) -> Vec<u32>;
            fn fetch(&mut self, mailbox: &str, uids: &[u32]) -> Vec<Message>;
            fn fetch_envelopes(&mut self, mailbox: &str, uids: &[u32]) -> Vec<Message>;
            fn recipients(&mut self, mailbox: &str, uids: &[u32]) -> Vec<String>;
            fn move_messages(&mut self, mailbox: &str, uids: &[u32], dest: &str) -> ();
            fn add_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> ();
            fn remove_keyword(&mut self, mailbox: &str, uids: &[u32], keyword: &str) -> ();
            fn ensure(&mut self, mailbox: &str) -> ();
            fn exists(&mut self, mailbox: &str) -> bool;
            fn folders(&mut self) -> Vec<String>;
        }
    }

    struct TestConnector {
        mail: Arc<Mutex<Fake>>,
        connects: Arc<AtomicUsize>,
        failures: Arc<StdMutex<VecDeque<mail::Error>>>,
        triggers: Arc<StdMutex<Vec<Trigger>>>,
    }

    impl Connector for TestConnector {
        type Mailbox = Shared;

        async fn connect(&mut self, _: &Config) -> mail::Result<Shared> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            match self.failures.lock().unwrap().pop_front() {
                Some(err) => Err(err),
                None => Ok(Shared(Arc::clone(&self.mail))),
            }
        }

        async fn watch(&mut self, _: &Config) -> Option<Watcher> {
            let (watcher, trigger) = Watcher::detached();
            self.triggers.lock().unwrap().push(trigger);
            Some(watcher)
        }
    }

    struct Env {
        daemon: Option<Daemon<TestConnector>>,
        mail: Arc<Mutex<Fake>>,
        store: Arc<Store>,
        connects: Arc<AtomicUsize>,
        failures: Arc<StdMutex<VecDeque<mail::Error>>>,
        triggers: Arc<StdMutex<Vec<Trigger>>>,
        shutdown: CancellationToken,
        hangup: Arc<Notify>,
        reloads: Arc<AtomicUsize>,
        status: Arc<Status>,
        _dir: tempfile::TempDir,
    }

    fn config() -> Config {
        let mut cfg = Config {
            account: Account {
                host: "imap.example.org".to_owned(),
                user: "me@example.org".to_owned(),
                ..Account::default()
            },
            ..Config::default()
        };
        cfg.policy.mode = Mode::Live;
        cfg
    }

    impl Env {
        fn new(reload: impl FnMut() -> Result<Config, config::Error> + Send + 'static) -> Self {
            Self::with(config(), reload)
        }

        fn with(
            cfg: Config,
            mut reload: impl FnMut() -> Result<Config, config::Error> + Send + 'static,
        ) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open(&dir.path().join("state.db")).unwrap());
            let mail = Arc::new(Mutex::new(Fake::new(&[
                "INBOX",
                "Junk",
                "Sent Messages",
                "TrainSpam",
                "TrainGood",
            ])));
            let connects = Arc::new(AtomicUsize::new(0));
            let failures = Arc::new(StdMutex::new(VecDeque::new()));
            let triggers = Arc::new(StdMutex::new(Vec::new()));
            let reloads = Arc::new(AtomicUsize::new(0));
            let shutdown = CancellationToken::new();
            let hangup = Arc::new(Notify::new());
            let status = Arc::new(Status::new(Timestamp::now()));

            let counted = Arc::clone(&reloads);
            let reload: Reload = Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                reload()
            });
            let connector = TestConnector {
                mail: Arc::clone(&mail),
                connects: Arc::clone(&connects),
                failures: Arc::clone(&failures),
                triggers: Arc::clone(&triggers),
            };
            let daemon = Daemon::new(
                connector,
                Arc::clone(&store),
                Arc::new(cfg),
                reload,
                shutdown.clone(),
                Arc::clone(&hangup),
            )
            .with_status(Arc::clone(&status));
            Self {
                daemon: Some(daemon),
                status,
                mail,
                store,
                connects,
                failures,
                triggers,
                shutdown,
                hangup,
                reloads,
                _dir: dir,
            }
        }

        fn connects(&self) -> usize {
            self.connects.load(Ordering::SeqCst)
        }

        fn fail_next(&self, err: mail::Error) {
            self.failures.lock().unwrap().push_back(err);
        }

        /// Runs the daemon in the background.
        fn spawn(&mut self) -> tokio::task::JoinHandle<()> {
            let mut daemon = self.daemon.take().unwrap();
            tokio::spawn(async move { daemon.run().await })
        }

        async fn stop(&self, handle: tokio::task::JoinHandle<()>) {
            self.shutdown.cancel();
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("the daemon did not stop")
                .unwrap();
        }
    }

    fn after(minutes: u64) -> impl Future<Output = ()> {
        tokio::time::sleep(Duration::from_mins(minutes))
    }

    fn mail_item(id: &str) -> String {
        format!(
            "From: Zed <zed@nowhere.example>\r\nTo: me@example.org\r\nSubject: Zzyzx\r\nMessage-ID: <{id}@x>\r\n\r\nqqqq wwww"
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_pass_returns_what_it_did() {
        let mut env = Env::new(|| unreachable!());
        let mut daemon = env.daemon.take().unwrap();

        assert_eq!(daemon.once().await.unwrap(), engine::Summary::default());
        env.mail.lock().await.deliver("INBOX", &mail_item("a"), &[]);
        let summary = daemon.once().await.unwrap();

        assert_eq!((summary.scanned, summary.uncertain), (1, 1));
        assert_eq!(env.connects(), 2, "each pass dials afresh");
    }

    #[tokio::test(start_paused = true)]
    async fn passes_repeat_every_poll_interval_until_shut_down() {
        let mut env = Env::new(|| unreachable!());
        let handle = env.spawn();

        after(5).await;
        assert_eq!(env.connects(), 3, "passes at 0, 2 and 4 minutes");

        env.stop(handle).await;
        let stopped_at = env.connects();
        after(10).await;
        assert_eq!(env.connects(), stopped_at, "nothing runs after shutdown");
    }

    #[tokio::test(start_paused = true)]
    async fn news_from_the_watcher_brings_the_next_pass_forward() {
        let mut env = Env::new(|| unreachable!());
        let handle = env.spawn();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(env.connects(), 1);
        assert_eq!(
            env.triggers.lock().unwrap().len(),
            1,
            "watching starts after the first good pass"
        );

        env.mail.lock().await.deliver("INBOX", &mail_item("a"), &[]);
        env.triggers.lock().unwrap()[0].fire();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            env.connects(),
            1,
            "a short pause lets a burst of mail settle"
        );

        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(env.connects(), 2);
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn failed_passes_back_off() {
        let mut env = Env::new(|| unreachable!());
        for _ in 0..2 {
            env.fail_next(mail::Error::Connection("down".into()));
        }
        let handle = env.spawn();

        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(env.connects(), 1, "the retry waits about the poll interval");
        after(3).await;
        assert_eq!(
            env.connects(),
            2,
            "the first retry has happened, the second waits longer"
        );
        after(10).await;
        assert!(env.connects() >= 3, "and the pass works again");
        assert!(
            env.triggers.lock().unwrap().len() == 1,
            "watching starts once a pass works"
        );
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_login_waits_long_and_sighup_retries_at_once() {
        let mut env = Env::new(|| Ok(config()));
        env.fail_next(mail::Error::Login("nope".into()));
        let handle = env.spawn();

        after(10).await;
        assert_eq!(env.connects(), 1, "not tried again for 15 minutes");

        env.hangup.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(env.connects(), 2);
        assert_eq!(env.reloads.load(Ordering::SeqCst), 1);
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn sighup_swaps_in_the_new_configuration_or_keeps_the_old() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&attempts);
        let mut env = Env::new(move || {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(config::Error::Invalid("broken".to_owned()))
            } else {
                let mut fresh = config();
                fresh.policy.poll_interval = Duration::from_mins(10);
                Ok(fresh)
            }
        });
        let handle = env.spawn();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(env.connects(), 1);

        env.hangup.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            env.connects(),
            2,
            "a reload is followed by a pass, even when it fails"
        );

        after(5).await;
        assert_eq!(env.connects(), 4, "the old two-minute poll still applies");

        env.hangup.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let before = env.connects();
        assert_eq!(before, 5);

        after(5).await;
        assert_eq!(env.connects(), before, "the new ten-minute poll applies");
        assert_eq!(
            env.triggers.lock().unwrap().len(),
            2,
            "the watcher is restarted after a reload"
        );
        env.stop(handle).await;
    }

    fn old_decision(seconds_ago: u64) -> Decision {
        Decision {
            at: Timestamp::now() - Duration::from_secs(seconds_ago),
            message_id: "<old@x>".to_owned(),
            ..Decision::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn old_decisions_are_pruned_at_most_once_a_day() {
        let year = 365 * 24 * 3600;
        let mut env = Env::new(|| unreachable!());
        env.store
            .record_decision(&old_decision(year + 3600))
            .unwrap();
        env.store.record_decision(&old_decision(60)).unwrap();
        let handle = env.spawn();

        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            env.store.stats().unwrap().decisions,
            1,
            "pruned when the daemon starts"
        );

        env.store
            .record_decision(&old_decision(year + 3600))
            .unwrap();
        after(60).await;
        assert_eq!(
            env.store.stats().unwrap().decisions,
            2,
            "not again within the day"
        );

        after(24 * 60).await;
        assert_eq!(env.store.stats().unwrap().decisions, 1);
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn zero_retention_keeps_everything() {
        let mut cfg = config();
        cfg.policy.journal_retention = Duration::ZERO;
        let mut env = Env::with(cfg, || unreachable!());
        env.store
            .record_decision(&old_decision(10 * 365 * 24 * 3600))
            .unwrap();
        let handle = env.spawn();

        after(5).await;

        assert_eq!(env.store.stats().unwrap().decisions, 1);
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_broken_rules_script_fails_the_pass_without_stopping_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("rules.sieve");
        std::fs::write(&script, "this is not sieve").unwrap();
        let mut cfg = config();
        cfg.rules.script = script.to_string_lossy().into_owned();
        let mut env = Env::with(cfg, || unreachable!());
        let handle = env.spawn();

        after(4).await;
        assert_eq!(
            env.connects(),
            2,
            "it retries with backoff while the script is broken"
        );
        after(20).await;
        assert!(env.connects() >= 4, "and keeps retrying");

        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_status_follows_the_passes() {
        let mut env = Env::new(|| unreachable!());
        let handle = env.spawn();

        after(5).await;
        let snapshot = env.status.snapshot();

        assert_eq!(snapshot.passes, 3);
        assert_eq!(
            snapshot.last_summary,
            "scanned 0, spam 0, uncertain 0, good 0, corrected 0, trained 0, rescued 0"
        );
        assert_eq!(
            (snapshot.failures, snapshot.waiting),
            (0, Some(Waiting::Polling)),
            "nothing is idling here"
        );
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_status_records_failures_and_when_the_next_try_is_due() {
        let mut env = Env::new(|| unreachable!());
        env.fail_next(mail::Error::Connection("down".into()));
        let handle = env.spawn();
        tokio::time::sleep(Duration::from_secs(10)).await;

        let failing = env.status.snapshot();
        assert_eq!(
            (failing.passes, failing.failures, failing.rejected),
            (0, 1, false)
        );
        assert!(
            failing.last_error.contains("down"),
            "{}",
            failing.last_error
        );
        assert!(failing.retry_at.is_some() && failing.failing_since.is_some());

        after(5).await;
        let recovered = env.status.snapshot();
        assert_eq!((recovered.failures, recovered.retry_at), (0, None));
        assert!(recovered.recovered_at.is_some());
        env.stop(handle).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_login_is_marked_in_the_status() {
        let mut env = Env::new(|| unreachable!());
        env.fail_next(mail::Error::Login("nope".into()));
        let handle = env.spawn();
        tokio::time::sleep(Duration::from_secs(10)).await;

        assert!(env.status.snapshot().rejected);
        env.stop(handle).await;
    }
}

#![allow(
    clippy::unused_async_trait_impl,
    reason = "the connector trait is async; the test double has nothing to await"
)]

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
    assert!(env.connects() >= 3, "and keeps retrying");

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
    assert_eq!(snapshot.failures, 0);
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

mod quality {
    use super::*;
    use crate::classify::Label;
    use crate::monitor::test_server::Server;

    const SETTLE: Duration = Duration::from_millis(300);

    fn daemon_pinging(env: &mut Env, server: &Server) -> Daemon<TestConnector> {
        let pinger = Pinger::new(&server.url).unwrap();
        env.daemon.take().unwrap().with_quality(pinger)
    }

    fn train_both_classes(store: &Store) {
        let tokens = vec!["word".to_owned()];
        store.train("spam", &tokens, Label::Spam).unwrap();
        store.train("good", &tokens, Label::Good).unwrap();
    }

    #[tokio::test]
    async fn without_monitoring_the_check_leaves_its_schedule_alone() {
        let env = Env::new(|| Ok(config()));

        env.daemon.as_ref().unwrap().check_quality();

        assert!(
            monitor::take_quality_slot(&env.store, Timestamp::now()).unwrap(),
            "the slot was not taken"
        );
    }

    #[tokio::test]
    async fn a_training_set_missing_a_class_pings_the_failure() {
        let mut env = Env::new(|| Ok(config()));
        let server = Server::start(&[]).await;

        let daemon = daemon_pinging(&mut env, &server);
        daemon.check_quality();
        server.wait_for(1).await;

        let sent = server.requests();
        assert_eq!(sent[0].path, "/fail");
        assert_eq!(
            sent[0].body,
            "FAIL class balance: 0 spam, 0 good: it needs examples of both\n"
        );
    }

    #[tokio::test]
    async fn a_healthy_filter_pings_success() {
        let mut env = Env::new(|| Ok(config()));
        train_both_classes(&env.store);
        let server = Server::start(&[]).await;

        let daemon = daemon_pinging(&mut env, &server);
        daemon.check_quality();
        server.wait_for(1).await;

        let sent = server.requests();
        assert_eq!(sent[0].path, "/");
        assert_eq!(sent[0].body, "the training and the decisions look healthy");
    }

    #[tokio::test]
    async fn the_check_pings_once_a_day() {
        let mut env = Env::new(|| Ok(config()));
        let server = Server::start(&[]).await;
        let daemon = daemon_pinging(&mut env, &server);

        daemon.check_quality();
        server.wait_for(1).await;
        daemon.check_quality();
        tokio::time::sleep(SETTLE).await;

        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn the_check_waits_for_a_slot_someone_else_took() {
        let mut env = Env::new(|| Ok(config()));
        let server = Server::start(&[]).await;
        let daemon = daemon_pinging(&mut env, &server);
        monitor::take_quality_slot(&env.store, Timestamp::now()).unwrap();

        daemon.check_quality();
        tokio::time::sleep(SETTLE).await;

        assert!(server.requests().is_empty(), "{:?}", server.requests());
    }
}

#[tokio::test(start_paused = true)]
async fn a_stop_does_not_wait_forever_for_a_stuck_pass() {
    let mut env = Env::new(|| Ok(config()));
    let mail = Arc::clone(&env.mail);
    let stuck = mail.lock().await;
    let handle = env.spawn();
    tokio::time::sleep(Duration::from_secs(1)).await;

    env.shutdown.cancel();

    tokio::time::timeout(STOP_GRACE + Duration::from_secs(1), handle)
        .await
        .expect("the daemon waited on a stuck pass")
        .unwrap();
    drop(stuck);
}

#[tokio::test(start_paused = true)]
async fn a_single_pass_cut_short_by_a_stop_is_not_an_error() {
    let mut env = Env::new(|| unreachable!());
    let mut daemon = env.daemon.take().unwrap();
    daemon.once().await.unwrap();
    env.mail.lock().await.deliver("INBOX", &mail_item("a"), &[]);
    env.shutdown.cancel();

    daemon.once().await.unwrap();

    assert!(
        env.store.decisions(10).unwrap().is_empty(),
        "the mail waits for the next pass"
    );
}

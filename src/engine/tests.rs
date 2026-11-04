use tempfile::TempDir;

use crate::classify::Label;
use crate::config::{Account, Config, Mode};
use crate::mail::fake::Fake;
use crate::mail::{Error as MailError, Mailbox};
use crate::rules::Rules;
use crate::store::{Decision, Store};

use super::*;

const FOLDERS: [&str; 5] = ["INBOX", "Junk", "Sent Messages", "TrainSpam", "TrainGood"];

struct Env {
    engine: Engine<Fake>,
    _dir: TempDir,
}

impl Env {
    fn new(mode: Mode) -> Self {
        Self::with(mode, |_| {})
    }

    fn with(mode: Mode, tweak: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("state.db")).unwrap());

        let mut cfg = Config {
            account: Account {
                host: "imap.example.org".to_owned(),
                user: "me@example.org".to_owned(),
                ..Account::default()
            },
            ..Config::default()
        };
        cfg.policy.mode = mode;
        tweak(&mut cfg);

        let engine = Engine::new(Fake::new(&FOLDERS), store, Arc::new(cfg));
        Self { engine, _dir: dir }
    }

    fn mail(&mut self) -> &mut Fake {
        &mut self.engine.mail
    }

    fn store(&self) -> &Store {
        &self.engine.store
    }

    fn train(&self, label: Label, raw: &str, key: &str) {
        let (tokens, _) = self.engine.features(raw.as_bytes()).unwrap();
        self.store().train(key, &tokens, label).unwrap();
    }

    /// A model that knows spam from good mail.
    fn trained() -> Self {
        Self::trained_with(Mode::Live, |_| {})
    }

    fn trained_with(mode: Mode, tweak: impl FnOnce(&mut Config)) -> Self {
        let env = Self::with(mode, tweak);
        for n in 0..5 {
            env.train(
                Label::Spam,
                &spam(&format!("t-spam-{n}")),
                &format!("<t-spam-{n}@x>"),
            );
            env.train(
                Label::Good,
                &good(&format!("t-good-{n}")),
                &format!("<t-good-{n}@x>"),
            );
        }
        env
    }

    /// Starts every watched folder at its current mail, then runs a pass.
    async fn start(&mut self) {
        self.engine.run_once().await.unwrap();
    }

    fn decisions(&self) -> Vec<Decision> {
        self.store().decisions(100).unwrap()
    }
}

fn message(id: &str, from: &str, subject: &str, body: &str, extra: &str) -> String {
    format!(
        "From: {from}\r\nTo: me@example.org\r\nSubject: {subject}\r\nMessage-ID: <{id}@x>\r\n{extra}\r\n{body}"
    )
}

fn spam(id: &str) -> String {
    message(
        id,
        "Prize <winner@lottery.example>",
        "Cheap pills winner",
        "free prize cheap pills winner claim",
        "",
    )
}

fn good(id: &str) -> String {
    message(
        id,
        "Alice <alice@friends.example>",
        "Lunch on Thursday",
        "lunch thursday meeting agenda",
        "",
    )
}

fn unknown(id: &str) -> String {
    message(id, "Zed <zed@nowhere.example>", "Zzyzx", "qqqq wwww", "")
}

#[tokio::test]
async fn a_new_folder_starts_at_its_current_mail() {
    let mut env = Env::trained();
    env.mail().deliver("INBOX", &spam("old"), &[]);

    assert_eq!(
        env.engine.run_once().await.unwrap(),
        Summary::default(),
        "existing mail is left to bootstrap"
    );
    assert_eq!(env.store().cursor("INBOX").unwrap().unwrap().last_uid, 1);

    env.mail().deliver("INBOX", &spam("new"), &[]);
    let summary = env.engine.run_once().await.unwrap();
    assert_eq!((summary.scanned, summary.spam), (1, 1));
}

#[tokio::test]
async fn a_changed_uid_validity_restarts_the_folder() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("a"), &[]);
    env.mail().reset_uid_validity("INBOX", 2);

    assert_eq!(env.engine.run_once().await.unwrap().scanned, 0);
    assert_eq!(
        env.store().cursor("INBOX").unwrap().unwrap().uid_validity,
        2
    );
}

#[tokio::test]
async fn spam_is_only_recorded_in_shadow_mode() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    let uid = env.mail().deliver("INBOX", &spam("s1"), &[]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.spam, 1);
    assert_eq!(env.mail().uids("INBOX"), [uid], "nothing moved");
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.verdict.as_str(),
            decision.action.as_str(),
            decision.applied
        ),
        ("spam", "move:Junk", false)
    );
}

#[tokio::test]
async fn spam_is_moved_and_flagged_in_live_mode() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), Vec::<u32>::new());
    assert_eq!(
        env.mail().flags("Junk", 1),
        Some(vec!["$tamisfiled".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (decision.reason.as_str(), decision.applied),
        ("bayes", true)
    );
    assert!(decision.score >= 0.9);
}

#[tokio::test]
async fn doubtful_mail_is_flagged_and_good_mail_is_left_alone() {
    let mut env = Env::trained();
    env.start().await;
    let unsure = env.mail().deliver("INBOX", &unknown("u1"), &[]);
    let fine = env.mail().deliver("INBOX", &good("g1"), &[]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(
        (summary.uncertain, summary.good, summary.scanned),
        (1, 1, 2)
    );
    assert_eq!(
        env.mail().flags("INBOX", unsure),
        Some(vec!["$tamisuncertain".to_owned()])
    );
    assert_eq!(env.mail().flags("INBOX", fine), Some(vec![]));
    let actions: Vec<String> = env.decisions().iter().map(|d| d.action.clone()).collect();
    assert_eq!(actions, ["none", "keyword:$TamisUncertain"]);
}

#[tokio::test]
async fn a_failed_move_takes_the_keyword_back() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.mail().fail("move", MailError::Server("nope".into()));

    let err = env.engine.run_once().await.unwrap_err().to_string();

    assert!(err.contains("file spam"), "{err}");
    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
    assert!(
        env.decisions().is_empty(),
        "nothing is journalled for what did not happen"
    );
}

#[tokio::test]
async fn a_dropped_connection_is_reported_as_one() {
    let mut env = Env::new(Mode::Live);
    env.mail()
        .fail("state", MailError::Connection("gone".into()));

    assert!(env.engine.run_once().await.unwrap_err().is_connection());
}

#[tokio::test]
async fn moving_filed_spam_back_is_a_correction() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.engine.run_once().await.unwrap();

    env.mail()
        .move_messages("Junk", &[1], "INBOX")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!((summary.corrected, summary.scanned), (1, 1));
    assert_eq!(env.store().trained("<s1@x>").unwrap(), Some(Label::Good));
    assert!(env.store().has_address("winner@lottery.example").unwrap());
    let uid = env.mail().uids("INBOX")[0];
    assert_eq!(
        env.mail().flags("INBOX", uid),
        Some(vec![]),
        "the filed keyword is cleared"
    );
    assert_eq!(env.decisions()[0].action, "train:good");
}

#[tokio::test]
async fn a_filed_keyword_without_a_journalled_move_is_not_believed() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &["$TamisFiled"]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!((summary.corrected, summary.spam), (0, 1));
}

#[tokio::test]
async fn mail_tamis_already_handled_is_not_judged_again() {
    let mut env = Env::trained();
    env.start().await;
    env.mail()
        .deliver("INBOX", &spam("rescued"), &["$TamisRescued"]);
    env.mail()
        .deliver("INBOX", &spam("learnt"), &["$TamisTrained"]);
    env.train(Label::Spam, &spam("learnt"), "<learnt@x>");
    env.mail()
        .deliver("INBOX", &spam("reused"), &["$TamisTrained"]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(
        summary.scanned, 1,
        "only the Message-ID that was never trained is judged"
    );
}

#[tokio::test]
async fn a_sorted_keyword_counts_only_when_the_journal_shows_the_move() {
    let mut env = Env::trained();
    env.start().await;
    env.mail()
        .deliver("INBOX", &good("moved"), &["$TamisSorted"]);
    env.mail()
        .deliver("INBOX", &good("failed"), &["$TamisSorted"]);
    env.engine
        .record(
            "INBOX",
            &Message {
                message_id: "<moved@x>".into(),
                ..Message::default()
            },
            &Judgement {
                verdict: Verdict::Good,
                reason: Reason::Rule,
                score: 0.0,
            },
            &Action::Move("Lists".into()),
            true,
        )
        .unwrap();

    assert_eq!(env.engine.run_once().await.unwrap().scanned, 1);
}

#[tokio::test]
async fn a_whitelisted_sender_needs_authentication_when_asked_to() {
    let cases = [
        ("no header", "", true, "bayes"),
        (
            "passed",
            "Authentication-Results: mx.example; dmarc=pass header.from=friends.example\r\n",
            true,
            "whitelisted",
        ),
        ("not required", "", false, "whitelisted"),
    ];
    for (name, header, required, want) in cases {
        let mut env = Env::trained_with(Mode::Live, |cfg| {
            cfg.policy.whitelist_requires_auth = required;
        });
        env.store()
            .add_address("alice@friends.example", "sent")
            .unwrap();
        env.start().await;
        env.mail().deliver(
            "INBOX",
            &message(
                "w1",
                "Alice <alice@friends.example>",
                "Hello",
                "hello",
                header,
            ),
            &[],
        );

        env.engine.run_once().await.unwrap();

        assert_eq!(env.decisions()[0].reason, want, "{name}");
    }
}

const RULES: &str =
    r#"require "fileinto"; if header :contains "list-id" "golang" { fileinto "Lists"; }"#;

fn listed() -> String {
    message(
        "l1",
        "Alice <alice@friends.example>",
        "Go news",
        "news",
        "List-Id: <golang-nuts.example>\r\nAuthentication-Results: mx.example; dmarc=pass header.from=friends.example\r\n",
    )
}

fn env_with_rules(mode: Mode, tweak: impl FnOnce(&mut Config)) -> Env {
    let mut env = Env::trained_with(mode, tweak);
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.engine
        .set_rules(Rules::parse(RULES.as_bytes()).unwrap());
    env.mail().add_folder("Lists");
    env
}

#[tokio::test]
async fn good_mail_is_filed_by_the_rules_in_live_mode() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), Vec::<u32>::new());
    assert_eq!(
        env.mail().flags("Lists", 1),
        Some(vec!["$tamissorted".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.reason.as_str(),
            decision.action.as_str(),
            decision.applied
        ),
        ("rule", "move:Lists", true)
    );
}

#[tokio::test]
async fn the_rules_only_report_in_shadow_mode() {
    let mut env = env_with_rules(Mode::Shadow, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [1]);
    assert!(!env.decisions()[0].applied);
}

#[tokio::test]
async fn a_rule_naming_a_missing_folder_leaves_the_mail() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.mail().remove_folder("Lists");
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [1], "the mail stays");
    assert_eq!(env.decisions()[0].action, "none");
}

#[tokio::test]
async fn a_rule_may_create_its_folder_when_allowed() {
    let mut env = env_with_rules(Mode::Live, |cfg| cfg.rules.create_folders = true);
    env.mail().remove_folder("Lists");
    env.start().await;
    env.mail().deliver("INBOX", &listed(), &[]);

    env.engine.run_once().await.unwrap();

    assert!(env.mail().exists("Lists").await.unwrap());
    assert_eq!(env.mail().uids("Lists"), [1]);
}

#[tokio::test]
async fn a_failed_rule_move_clears_the_sorted_keyword() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.start().await;
    let uid = env.mail().deliver("INBOX", &listed(), &[]);
    env.mail().fail("move", MailError::Server("nope".into()));

    assert!(env.engine.run_once().await.is_err());
    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
}

#[tokio::test]
async fn missing_rule_folders_are_reported_without_failing() {
    let mut env = env_with_rules(Mode::Live, |_| {});
    env.mail().remove_folder("Lists");
    env.engine.warn_missing_rule_folders().await.unwrap();

    let mut quiet = env_with_rules(Mode::Live, |cfg| cfg.rules.create_folders = true);
    quiet.engine.warn_missing_rule_folders().await.unwrap();
    assert!(
        !quiet
            .mail()
            .log
            .iter()
            .any(|call| call.starts_with("folders")),
        "nothing to check when folders are created"
    );
}

#[test]
fn the_authserv_id_comes_from_config_then_from_what_was_learnt() {
    #[allow(clippy::type_complexity, reason = "a table of cases")]
    let cases: [(&str, &str, Option<(&str, &str)>, &str, AuthSource); 6] = [
        ("off", "none", None, "", AuthSource::Off),
        (
            "configured",
            "icloud.com",
            Some(("learnt.example", "imap.example.org")),
            "icloud.com",
            AuthSource::Configured,
        ),
        ("unset", "", None, "", AuthSource::Unset),
        (
            "learnt",
            "",
            Some(("icloud.com", "IMAP.example.org")),
            "icloud.com",
            AuthSource::Learnt,
        ),
        (
            "learnt before hosts were recorded",
            "",
            Some(("icloud.com", "")),
            "icloud.com",
            AuthSource::Learnt,
        ),
        (
            "stale",
            "",
            Some(("icloud.com", "imap.other.example")),
            "",
            AuthSource::Stale,
        ),
    ];
    for (name, configured, learnt, id, source) in cases {
        let env = Env::with(Mode::Live, |cfg| {
            cfg.account.auth_server_id = configured.to_owned();
        });
        if let Some((domain, host)) = learnt {
            env.store().set_setting("auth_server_id", domain).unwrap();
            if !host.is_empty() {
                env.store()
                    .set_setting("auth_server_id_host", host)
                    .unwrap();
            }
        }

        let pin = resolve_auth_pin(env.store(), &env.engine.cfg).unwrap();

        assert_eq!((pin.id.as_str(), pin.source), (id, source), "{name}");
    }
}

#[test]
fn training_keys_fall_back_to_a_content_hash() {
    let with_id = Message {
        message_id: "<a@x>".into(),
        raw: b"x".to_vec(),
        ..Message::default()
    };
    let without = Message {
        raw: b"abc".to_vec(),
        ..Message::default()
    };

    assert_eq!(training_key(&with_id), "<a@x>");
    assert_eq!(
        training_key(&without),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[tokio::test]
async fn a_message_dropped_in_a_training_folder_is_learnt_and_moved_on() {
    for mode in [Mode::Live, Mode::Shadow] {
        let mut env = Env::with(mode, |_| {});
        let spam_uid = env.mail().deliver("TrainSpam", &spam("ts1"), &[]);
        let good_uid = env.mail().deliver("TrainGood", &good("tg1"), &[]);

        let summary = env.engine.run_once().await.unwrap();

        assert_eq!(summary.trained, 2, "{mode:?}");
        assert_eq!(env.store().trained("<ts1@x>").unwrap(), Some(Label::Spam));
        assert_eq!(env.store().trained("<tg1@x>").unwrap(), Some(Label::Good));
        assert!(env.store().has_address("alice@friends.example").unwrap());
        assert_eq!(
            env.mail().flags("Junk", 1),
            Some(vec!["$tamistrained".to_owned()])
        );
        assert_eq!(
            env.mail().flags("INBOX", 1),
            Some(vec!["$tamistrained".to_owned()])
        );
        assert!(env.mail().uids("TrainSpam").is_empty() && env.mail().uids("TrainGood").is_empty());
        let _ = (spam_uid, good_uid);

        let reasons: Vec<(String, String, bool)> = env
            .decisions()
            .iter()
            .map(|d| (d.reason.clone(), d.action.clone(), d.applied))
            .collect();
        assert!(
            reasons.contains(&("train folder".into(), "train:spam".into(), true)),
            "{reasons:?}"
        );
        assert!(
            reasons.contains(&("train folder".into(), "train:good".into(), true)),
            "{reasons:?}"
        );
    }
}

#[tokio::test]
async fn trained_mail_that_lands_in_the_inbox_is_not_judged_again() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("TrainGood", &good("tg1"), &[]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!((summary.trained, summary.scanned), (1, 0));
}

#[tokio::test]
async fn empty_training_folders_cost_one_status() {
    let mut env = Env::new(Mode::Live);

    env.engine.run_once().await.unwrap();

    let log = &env.mail().log;
    assert!(log.contains(&"messages TrainSpam".to_owned()));
    assert!(
        !log.iter()
            .any(|call| call.ends_with("TrainSpam") && !call.starts_with("messages")),
        "{log:?}"
    );
}

#[tokio::test]
async fn sent_mail_whitelists_its_recipients_but_not_you() {
    let mut env = Env::new(Mode::Live);
    env.start().await;
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: Bob <Bob@Friends.example>, me@example.org\r\nCc: carol@friends.example\r\nSubject: hi\r\nMessage-ID: <s1@x>\r\n\r\nhi",
        &[],
    );

    env.engine.run_once().await.unwrap();

    let listed: Vec<String> = env
        .store()
        .addresses()
        .unwrap()
        .into_iter()
        .map(|a| a.address)
        .collect();
    assert_eq!(listed, ["bob@friends.example", "carol@friends.example"]);
}

#[tokio::test]
async fn answering_mail_tamis_doubted_teaches_that_it_is_good() {
    let cases = [
        ("spam in Junk", "spam", "Junk", true),
        ("uncertain in the inbox", "uncertain", "INBOX", true),
        ("good is left alone", "good", "INBOX", false),
        ("not judged at all", "", "INBOX", false),
    ];
    for (name, verdict, folder, learnt) in cases {
        let mut env = Env::trained();
        env.mail().deliver(folder, &spam("orig"), &[]);
        env.start().await;
        if !verdict.is_empty() {
            let message = Message {
                message_id: "<orig@x>".into(),
                from: "winner@lottery.example".into(),
                ..Message::default()
            };
            let judgement = Judgement {
                verdict: if verdict == "spam" {
                    Verdict::Spam
                } else if verdict == "good" {
                    Verdict::Good
                } else {
                    Verdict::Uncertain
                },
                reason: Reason::Bayes,
                score: 0.5,
            };
            env.engine
                .record("INBOX", &message, &judgement, &Action::None, false)
                .unwrap();
        }
        env.mail().deliver(
            "Sent Messages",
            "From: me@example.org\r\nTo: winner@lottery.example\r\nSubject: Re: hi\r\nMessage-ID: <reply@x>\r\nIn-Reply-To: <orig@x>\r\n\r\nthanks",
            &[],
        );

        env.engine.run_once().await.unwrap();

        let label = env.store().trained("<orig@x>").unwrap();
        assert_eq!(label == Some(Label::Good), learnt, "{name}");
    }
}

#[tokio::test]
async fn a_reply_to_vanished_mail_is_only_a_warning() {
    let mut env = Env::trained();
    env.start().await;
    let message = Message {
        message_id: "<gone@x>".into(),
        ..Message::default()
    };
    let judgement = Judgement {
        verdict: Verdict::Spam,
        reason: Reason::Bayes,
        score: 0.9,
    };
    env.engine
        .record("INBOX", &message, &judgement, &Action::None, false)
        .unwrap();
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: a@b.example\r\nMessage-ID: <r@x>\r\nIn-Reply-To: <gone@x>\r\n\r\nx",
        &[],
    );

    assert!(env.engine.run_once().await.is_ok());
}

#[tokio::test]
async fn a_sent_folder_that_cannot_be_scanned_does_not_fail_the_pass() {
    let mut env = Env::new(Mode::Live);
    env.mail().remove_folder("Sent Messages");
    assert!(env.engine.run_once().await.is_ok());

    let mut dropped = Env::new(Mode::Live);
    dropped
        .mail()
        .fail("state", MailError::Server("fine".into()));
    dropped
        .mail()
        .fail("state", MailError::Connection("gone".into()));
    let err = dropped.engine.run_once().await.unwrap_err();
    assert!(
        err.to_string().contains("scan INBOX") || err.is_connection(),
        "{err}"
    );
}

fn corpus(env: &mut Env) {
    let old = jiff::Timestamp::from_second(1_000).unwrap();
    env.mail()
        .deliver_at("INBOX", &good("read-old"), &["\\Seen"], old);
    env.mail()
        .deliver_at("INBOX", &good("unread-old"), &[], old);
    env.mail().deliver("INBOX", &good("read-new"), &["\\Seen"]);
    env.mail().deliver("Junk", &spam("j1"), &[]);
    env.mail().deliver("Junk", &spam("j2"), &[]);
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: bob@friends.example\r\nMessage-ID: <s1@x>\r\n\r\nhi",
        &[],
    );
}

fn options() -> BootstrapOptions {
    BootstrapOptions {
        sent_limit: 10,
        good: vec![Source {
            folder: "INBOX".into(),
            limit: 10,
        }],
        spam: vec![Source {
            folder: "Junk".into(),
            limit: 1,
        }],
        good_min_age: std::time::Duration::from_secs(3600),
        ..BootstrapOptions::default()
    }
}

#[tokio::test]
async fn bootstrap_learns_old_read_mail_and_the_latest_spam() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);

    let summary = env.engine.bootstrap(&options()).await.unwrap();

    assert_eq!((summary.addresses, summary.good, summary.spam), (1, 1, 1));
    assert_eq!(
        env.store().trained("<read-old@x>").unwrap(),
        Some(Label::Good)
    );
    assert_eq!(
        env.store().trained("<unread-old@x>").unwrap(),
        None,
        "unread mail may be spam nobody has seen"
    );
    assert_eq!(
        env.store().trained("<read-new@x>").unwrap(),
        None,
        "recent mail is too new"
    );
    assert_eq!(
        env.store().trained("<j1@x>").unwrap(),
        None,
        "only the latest are taken"
    );
    assert_eq!(env.store().trained("<j2@x>").unwrap(), Some(Label::Spam));
    assert!(env.store().has_address("bob@friends.example").unwrap());
    assert_eq!(
        env.store().cursor("INBOX").unwrap().unwrap().last_uid,
        3,
        "watching starts from current mail"
    );
}

#[tokio::test]
async fn bootstrap_can_leave_the_cursors_alone() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);

    env.engine
        .bootstrap(&BootstrapOptions {
            keep_cursors: true,
            ..options()
        })
        .await
        .unwrap();

    assert_eq!(env.store().cursor("INBOX").unwrap(), None);
}

#[tokio::test]
async fn refreshing_learns_what_was_learnt_again() {
    let mut env = Env::new(Mode::Live);
    corpus(&mut env);
    env.engine.bootstrap(&options()).await.unwrap();

    let again = env.engine.bootstrap(&options()).await.unwrap();
    assert_eq!(
        (again.good, again.spam, again.refreshed),
        (0, 0, 0),
        "nothing changed"
    );

    let refreshed = env
        .engine
        .bootstrap(&BootstrapOptions {
            refresh: true,
            ..options()
        })
        .await
        .unwrap();
    assert_eq!(
        (refreshed.good, refreshed.spam, refreshed.refreshed),
        (1, 1, 2)
    );
}

#[tokio::test]
async fn bootstrap_learns_the_providers_authserv_id() {
    let mut env = Env::new(Mode::Live);
    let old = jiff::Timestamp::from_second(1_000).unwrap();
    for n in 0..20 {
        let raw = message(
            &format!("a{n}"),
            "Alice <alice@friends.example>",
            "hi",
            "hello",
            "Authentication-Results: mx.icloud.com; dmarc=pass header.from=friends.example\r\n",
        );
        env.mail().deliver_at("INBOX", &raw, &["\\Seen"], old);
    }

    let wide = BootstrapOptions {
        good: vec![Source {
            folder: "INBOX".into(),
            limit: 50,
        }],
        ..options()
    };
    let summary = env.engine.bootstrap(&wide).await.unwrap();

    assert_eq!(summary.auth_server_id.as_deref(), Some("icloud.com"));
    let pin = resolve_auth_pin(env.store(), &env.engine.cfg).unwrap();
    assert_eq!(
        (pin.id.as_str(), pin.source),
        ("icloud.com", AuthSource::Learnt)
    );
}

#[tokio::test]
async fn a_failing_corpus_search_stops_bootstrap() {
    let mut env = Env::new(Mode::Live);
    env.mail()
        .fail("seen_before", MailError::Server("nope".into()));

    let err = env
        .engine
        .bootstrap(&options())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("bootstrap from INBOX"), "{err}");
}

fn authenticated(id: &str, from: &str) -> String {
    message(
        id,
        from,
        "Hello",
        "hello there",
        "Authentication-Results: mx.example; dmarc=pass header.from=friends.example\r\n",
    )
}

#[tokio::test]
async fn mail_the_user_moves_to_junk_is_learnt_as_missed_spam() {
    let mut env = Env::trained();
    env.store()
        .add_address("zed@nowhere.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver("INBOX", &unknown("m1"), &[]);
    env.engine.run_once().await.unwrap();
    let uid = env.mail().uids("INBOX")[0];

    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.corrected, 1);
    assert_eq!(env.store().trained("<m1@x>").unwrap(), Some(Label::Spam));
    assert!(
        !env.store().has_address("zed@nowhere.example").unwrap(),
        "the sender comes off the whitelist"
    );
    assert_eq!(env.decisions()[0].reason, "correction");
}

#[tokio::test]
async fn spam_tamis_filed_is_not_a_correction_when_it_reaches_junk() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.engine.run_once().await.unwrap();
    let before = env.decisions().len();

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(
        (summary.corrected, summary.trained, summary.rescued),
        (0, 0, 0)
    );
    assert_eq!(env.decisions().len(), before);
}

#[tokio::test]
async fn a_rescue_moved_back_to_junk_is_learnt_as_spam() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("INBOX").len(), 1, "rescued");

    let uid = env.mail().uids("INBOX")[0];
    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.corrected, 1);
    assert_eq!(env.store().trained("<r1@x>").unwrap(), Some(Label::Spam));
    assert!(!env.store().has_address("alice@friends.example").unwrap());
}

#[tokio::test]
async fn whitelisted_authenticated_mail_is_rescued_from_junk() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.rescued, 1);
    assert!(env.mail().uids("Junk").is_empty());
    assert_eq!(
        env.mail().flags("INBOX", 1),
        Some(vec!["$tamisrescued".to_owned()])
    );
    let decision = &env.decisions()[0];
    assert_eq!(
        (
            decision.reason.as_str(),
            decision.action.as_str(),
            decision.applied
        ),
        ("whitelisted", "move:INBOX", true)
    );

    assert_eq!(
        env.engine.run_once().await.unwrap().scanned,
        0,
        "a rescue is not judged again in the inbox"
    );
}

#[tokio::test]
async fn rescue_needs_authentication_even_when_the_whitelist_does_not() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.whitelist_requires_auth = false);
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &message("r1", "Alice <alice@friends.example>", "Hello", "hello", ""),
        &[],
    );

    assert_eq!(env.engine.run_once().await.unwrap().rescued, 0);
}

#[tokio::test]
async fn rescue_only_reports_in_shadow_mode_and_can_go_to_its_own_folder() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("Junk"), [1]);
    assert!(!env.decisions()[0].applied);

    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.folders.rescued = "Rescued".to_owned());
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("Rescued"), [1], "the folder is created");
}

#[tokio::test]
async fn a_very_low_score_rescues_when_a_threshold_is_set() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.rescue_threshold = 0.1);
    env.start().await;
    env.mail().deliver("Junk", &good("g1"), &[]);
    env.mail().deliver("Junk", &unknown("u1"), &[]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.rescued, 1);
    assert_eq!(env.mail().uids("Junk").len(), 1);
    assert_eq!(
        env.decisions()
            .iter()
            .find(|d| d.action == "move:INBOX")
            .unwrap()
            .reason,
        "bayes"
    );
}

#[tokio::test]
async fn a_failed_rescue_move_clears_its_keyword() {
    let mut env = Env::trained();
    env.store()
        .add_address("alice@friends.example", "sent")
        .unwrap();
    env.start().await;
    let uid = env.mail().deliver(
        "Junk",
        &authenticated("r1", "Alice <alice@friends.example>"),
        &[],
    );
    env.mail().fail("move", MailError::Server("nope".into()));

    assert!(env.engine.run_once().await.is_err());
    assert_eq!(env.mail().flags("Junk", uid), Some(vec![]));
}

#[tokio::test]
async fn provider_junk_teaches_the_model_unless_tamis_is_already_sure() {
    let mut env = Env::trained_with(Mode::Live, |cfg| cfg.policy.rescue = false);
    env.store()
        .add_address("zed@nowhere.example", "sent")
        .unwrap();
    env.start().await;
    env.mail().deliver("Junk", &unknown("p1"), &[]);
    env.mail().deliver("Junk", &spam("p2"), &[]);
    env.mail().deliver("Junk", &unknown("p3"), &["$Junk"]);

    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.trained, 1);
    assert_eq!(env.store().trained("<p1@x>").unwrap(), Some(Label::Spam));
    assert_eq!(
        env.store().trained("<p2@x>").unwrap(),
        None,
        "already sure: nothing to learn"
    );
    assert_eq!(
        env.store().trained("<p3@x>").unwrap(),
        None,
        "marked Junk by the user"
    );
    assert!(
        env.store().has_address("zed@nowhere.example").unwrap(),
        "the whitelist is left alone"
    );
    assert_eq!(env.decisions()[0].reason, "provider junk");
}

#[tokio::test]
async fn junk_is_not_scanned_when_no_policy_wants_it() {
    let mut env = Env::with(Mode::Live, |cfg| {
        cfg.policy.learn_from_junk = false;
        cfg.policy.rescue = false;
        cfg.policy.learn_provider_junk = false;
    });

    env.engine.run_once().await.unwrap();

    assert!(!env.mail().log.contains(&"state Junk".to_owned()));
}

#[tokio::test]
async fn marks_that_contradict_tamis_are_learnt() {
    let cases = [
        ("Junk on good mail", good("k1"), "$Junk", Some(Label::Spam)),
        ("NotJunk on spam", spam("k1"), "$NotJunk", Some(Label::Good)),
        ("Apple Mail's Junk", good("k1"), "Junk", Some(Label::Spam)),
        ("a mark that agrees", spam("k1"), "$Junk", None),
    ];
    for (name, raw, mark, learnt) in cases {
        let mut env = Env::trained_with(Mode::Shadow, |_| {});
        env.start().await;
        let uid = env.mail().deliver("INBOX", &raw, &[]);
        env.engine.run_once().await.unwrap();
        env.mail().add_keyword("INBOX", &[uid], mark).await.unwrap();

        let summary = env.engine.run_once().await.unwrap();

        assert_eq!(env.store().trained("<k1@x>").unwrap(), learnt, "{name}");
        assert_eq!(summary.corrected, usize::from(learnt.is_some()), "{name}");
    }
}

#[tokio::test]
async fn ambiguous_and_unjudged_marks_are_ignored() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    let both = env.mail().deliver("INBOX", &good("k1"), &[]);
    env.engine.run_once().await.unwrap();
    env.mail()
        .add_keyword("INBOX", &[both], "$Junk")
        .await
        .unwrap();
    env.mail()
        .add_keyword("INBOX", &[both], "$NotJunk")
        .await
        .unwrap();
    env.mail()
        .deliver("INBOX", &good("never-judged"), &["$Junk"]);
    env.store()
        .set_cursor(
            "INBOX",
            crate::store::Cursor {
                uid_validity: 1,
                last_uid: 2,
            },
        )
        .unwrap();

    assert_eq!(env.engine.run_once().await.unwrap().corrected, 0);
}

#[tokio::test]
async fn with_condstore_unchanged_folders_are_not_searched_again() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.mail().condstore = true;
    env.start().await;
    let searches = |env: &mut Env| {
        env.mail()
            .log
            .iter()
            .filter(|c| *c == "search_keywords INBOX")
            .count()
    };
    let first = searches(&mut env);
    assert!(first > 0);

    env.engine.run_once().await.unwrap();
    assert_eq!(searches(&mut env), first, "nothing changed");

    let uid = env.mail().deliver("INBOX", &good("k1"), &[]);
    env.engine.run_once().await.unwrap();
    env.mail()
        .add_keyword("INBOX", &[uid], "$Junk")
        .await
        .unwrap();
    env.engine.run_once().await.unwrap();
    assert!(searches(&mut env) > first);
    assert_eq!(env.store().trained("<k1@x>").unwrap(), Some(Label::Spam));
}

#[tokio::test]
async fn keyword_learning_can_be_turned_off() {
    let mut env = Env::with(Mode::Live, |cfg| cfg.policy.learn_from_keywords = false);
    env.engine.run_once().await.unwrap();

    assert!(
        !env.mail()
            .log
            .iter()
            .any(|call| call.starts_with("search_keywords"))
    );
}

use super::*;

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
            decision.action.to_string().as_str(),
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
    let actions: Vec<String> = env
        .decisions()
        .iter()
        .map(|d| d.action.to_string())
        .collect();
    assert_eq!(actions, ["none", "keyword:$TamisUncertain"]);
}

#[tokio::test]
async fn a_failed_move_takes_the_keyword_back() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.mail().fail("move", MailError::Server("nope".into()));

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().flags("INBOX", uid), Some(vec![]));
    assert!(
        env.decisions().is_empty(),
        "nothing is journalled for what did not happen"
    );
}

#[tokio::test]
async fn a_message_the_server_refuses_to_move_does_not_block_the_ones_after_it() {
    let mut env = Env::trained();
    env.start().await;
    let stuck = env.mail().deliver("INBOX", &spam("s1"), &[]);
    let next = env.mail().deliver("INBOX", &spam("s2"), &[]);
    env.mail().fail("move", MailError::Server("nope".into()));

    env.engine.run_once().await.unwrap();

    assert_eq!(env.mail().uids("INBOX"), [stuck]);
    assert_eq!(env.mail().uids("Junk").len(), 1, "UID {next} was filed");
    env.engine.run_once().await.unwrap();
    assert_eq!(env.mail().uids("INBOX"), [stuck], "it is not retried");
}

#[tokio::test]
async fn a_dropped_connection_mid_batch_keeps_the_cursor_behind_the_message() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.mail()
        .fail("move", MailError::Connection("gone".into()));

    assert!(env.engine.run_once().await.unwrap_err().is_connection());
    env.engine.run_once().await.unwrap();
    assert!(!env.mail().uids("INBOX").contains(&uid));
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
async fn a_move_whose_reply_was_lost_is_journalled_on_the_next_pass() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.mail()
        .fail_after("move", MailError::Connection("gone".into()));
    assert!(env.engine.run_once().await.unwrap_err().is_connection());
    assert_eq!(env.mail().uids("Junk").len(), 1, "the server did move it");

    env.engine.run_once().await.unwrap();

    let decisions = env.decisions();
    assert_eq!(decisions.len(), 1);
    assert!(decisions[0].applied);
    assert!(env.store().pending_moves().unwrap().is_empty());

    env.mail()
        .move_messages("Junk", &[1], "INBOX")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();
    assert_eq!(summary.corrected, 1, "moving it back is learnt");
}

#[tokio::test]
async fn a_move_that_never_happened_is_forgotten_and_tried_again() {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.mail()
        .fail("move", MailError::Connection("gone".into()));
    assert!(env.engine.run_once().await.unwrap_err().is_connection());
    assert_eq!(env.store().pending_moves().unwrap().len(), 1);

    env.engine.run_once().await.unwrap();

    assert!(env.mail().uids("INBOX").is_empty());
    assert_eq!(env.mail().uids("Junk").len(), 1);
    let decisions = env.decisions();
    assert_eq!(
        decisions.len(),
        1,
        "no row left for the attempt that failed"
    );
    assert!(decisions[0].applied);
    assert!(env.store().pending_moves().unwrap().is_empty());
}

#[tokio::test]
async fn a_stale_filed_keyword_is_cleared_so_a_later_move_to_junk_is_missed_spam() {
    let mut env = Env::trained();
    env.start().await;
    let uid = env.mail().deliver("INBOX", &good("g1"), &["$TamisFiled"]);

    env.engine.run_once().await.unwrap();
    assert_eq!(
        env.mail().flags("INBOX", uid),
        Some(vec![]),
        "the stale keyword is cleared"
    );

    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();
    let summary = env.engine.run_once().await.unwrap();

    assert_eq!(summary.corrected, 1);
    assert_eq!(env.store().trained("<g1@x>").unwrap(), Some(Label::Spam));
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

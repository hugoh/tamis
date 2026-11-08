use super::*;

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
            .map(|d| (d.reason.to_string(), d.action.to_string(), d.applied))
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
async fn whitelisting_sent_recipients_is_journalled_once_per_message() {
    let mut env = Env::new(Mode::Live);
    env.start().await;
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: Bob <bob@friends.example>, me@example.org\r\nCc: carol@friends.example\r\nSubject: hi\r\nMessage-ID: <s1@x>\r\n\r\nhi",
        &[],
    );
    env.mail().deliver(
        "Sent Messages",
        "From: me@example.org\r\nTo: me@example.org\r\nSubject: note to self\r\nMessage-ID: <s2@x>\r\n\r\nhi",
        &[],
    );

    env.engine.run_once().await.unwrap();

    let decisions = env.store().decisions(10).unwrap();
    assert_eq!(decisions.len(), 1, "only the message with recipients");
    let entry = &decisions[0];
    assert_eq!(entry.mailbox, "Sent Messages");
    assert_eq!(entry.message_id, "<s1@x>");
    assert_eq!(entry.from, "bob@friends.example, carol@friends.example");
    assert_eq!(entry.subject, "hi");
    assert_eq!(entry.verdict, Verdict::Good);
    assert_eq!(entry.reason, Reason::Sent);
    assert!(entry.applied);
    assert!(!crate::engine::can_undo(entry));
    assert!(!crate::engine::can_learn(entry));
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

#[tokio::test]
async fn a_stop_during_training_waits_for_the_batch_then_resumes() {
    let mut env = Env::with(Mode::Live, |_| {});
    for n in 0..=FETCH_BATCH {
        env.mail()
            .deliver("TrainSpam", &spam(&format!("t{n}")), &[]);
    }
    let stop = CancellationToken::new();
    env.engine.set_shutdown(stop.clone());
    env.mail().cancel_on("move", stop);

    let err = env.engine.run_once().await.unwrap_err();

    assert!(err.is_stopped(), "{err}");
    assert_eq!(env.mail().uids("TrainSpam").len(), 1, "one batch was moved");

    env.engine.set_shutdown(CancellationToken::new());
    assert_eq!(env.engine.run_once().await.unwrap().trained, 1);
}

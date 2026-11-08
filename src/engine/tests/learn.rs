use super::*;

#[tokio::test]
async fn learning_from_a_shadow_decision_teaches_the_opposite() {
    let cases = [
        (
            "spam becomes good",
            spam("l1"),
            None,
            Label::Good,
            "learnt as good, sender whitelisted",
        ),
        (
            "good becomes spam",
            good("l1"),
            None,
            Label::Spam,
            "learnt as spam, sender removed from whitelist",
        ),
        (
            "a label overrides",
            good("l1"),
            Some(Label::Good),
            Label::Good,
            "learnt as good, sender whitelisted",
        ),
    ];
    for (name, raw, label, want, account) in cases {
        let mut env = Env::trained_with(Mode::Shadow, |_| {});
        env.start().await;
        env.mail().deliver("INBOX", &raw, &[]);
        env.engine.run_once().await.unwrap();
        let id = env.decisions()[0].id;

        assert_eq!(
            env.engine.learn(id, label).await.unwrap(),
            account,
            "{name}"
        );

        assert_eq!(env.store().trained("<l1@x>").unwrap(), Some(want), "{name}");
        let entry = &env.decisions()[0];
        assert_eq!(entry.reason, "correction", "{name}");
        assert_eq!(
            entry.action.to_string(),
            format!("train:{}", want.as_str()),
            "{name}"
        );
    }
}

#[tokio::test]
async fn uncertain_decisions_need_a_label() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    env.mail().deliver("INBOX", &unknown("u1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = env.decisions()[0].id;

    assert!(
        env.engine
            .learn(id, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("needs an explicit label")
    );
    env.engine.learn(id, Some(Label::Spam)).await.unwrap();
    assert!(
        env.engine
            .learn(id, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("already corrected")
    );
    env.engine.learn(id, Some(Label::Good)).await.unwrap();
}

#[tokio::test]
async fn learning_refuses_what_is_not_tamis_own_unapplied_judgement() {
    let (mut env, applied) = filed_spam().await;
    let message = Message {
        message_id: "<t@x>".into(),
        ..Message::default()
    };
    let judgement = Judgement {
        verdict: Verdict::Good,
        reason: Reason::TrainFolder,
        score: 0.0,
    };
    env.engine
        .record("INBOX", &message, &judgement, &Action::TrainGood, true)
        .unwrap();
    let training = decision_where(&env, |d| d.reason == "train folder");

    let cases = [
        (applied, "use undo"),
        (training, "only tamis's own judgements"),
    ];
    for (id, want) in cases {
        let err = env.engine.learn(id, None).await.unwrap_err().to_string();
        assert!(err.contains(want), "{id}: {err}");
    }
}

#[tokio::test]
async fn learning_looks_in_junk_when_the_message_was_moved_there() {
    let mut env = Env::trained_with(Mode::Shadow, |_| {});
    env.start().await;
    let uid = env.mail().deliver("INBOX", &good("l1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = env.decisions()[0].id;
    env.mail()
        .move_messages("INBOX", &[uid], "Junk")
        .await
        .unwrap();

    env.engine.learn(id, None).await.unwrap();
    assert_eq!(env.decisions()[0].mailbox, "Junk");

    env.mail()
        .move_messages("Junk", &[1], "TrainSpam")
        .await
        .unwrap();
    let id = env.decisions()[0].id;
    let err = env.engine.learn(id, Some(Label::Good)).await.unwrap_err();
    assert!(err.is_message_gone(), "{err}");
}

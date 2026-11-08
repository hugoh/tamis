use tempfile::TempDir;

use crate::auth_pin::{AuthSource, resolve_auth_pin};
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

fn decision_where(env: &Env, pick: impl Fn(&Decision) -> bool) -> i64 {
    env.decisions()
        .into_iter()
        .find(|d| pick(d))
        .map(|d| d.id)
        .unwrap()
}

async fn filed_spam() -> (Env, i64) {
    let mut env = Env::trained();
    env.start().await;
    env.mail().deliver("INBOX", &spam("s1"), &[]);
    env.engine.run_once().await.unwrap();
    let id = decision_where(&env, |d| d.action == "move:Junk");
    (env, id)
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

mod bootstrap;
mod junk;
mod learn;
mod passes;
mod rules;
mod training;
mod undo;

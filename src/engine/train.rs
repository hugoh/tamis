use mail_parser::MessageParser;

use jiff::Timestamp;

use crate::classify::Label;
use crate::mail::{Mailbox, Message};
use crate::store::Decision;

use super::domain::{Action, Judgement, Reason, Summary, Verdict, keyword};
use super::{Context, Engine, Error, FETCH_BATCH, Result, training_key};

/// A training folder: what it teaches and where its messages go next.
pub(super) struct TrainFolder {
    name: String,
    label: Label,
    dest: String,
}

impl<M: Mailbox> Engine<M> {
    /// `TrainSpam` empties into Junk, `TrainGood` back into the first watched
    /// folder.
    pub(super) fn train_folders(&self) -> [TrainFolder; 2] {
        let folders = &self.cfg.folders;
        [
            TrainFolder {
                name: folders.train_spam.clone(),
                label: Label::Spam,
                dest: folders.junk.clone(),
            },
            TrainFolder {
                name: folders.train_good.clone(),
                label: Label::Good,
                dest: folders.watch[0].clone(),
            },
        ]
    }

    /// Whether the folder exists and holds nothing, which a `STATUS` answers
    /// without selecting it. Any doubt, such as a missing folder, is false.
    async fn is_empty(&mut self, folder: &str) -> bool {
        matches!(self.mail.messages(folder).await, Ok(0))
    }

    /// Learns every message in a training folder, then moves it on. This is an
    /// explicit request from the user, so it runs in shadow mode too.
    pub(super) async fn train(
        &mut self,
        folder: &TrainFolder,
        summary: &mut Summary,
    ) -> Result<()> {
        if self.is_empty(&folder.name).await {
            return Ok(());
        }

        let context = format!("train from {}", folder.name);
        self.mail.ensure(&folder.name).await.context(&context)?;
        let uids = self
            .mail
            .uids_after(&folder.name, 0)
            .await
            .context(&context)?;

        for batch in uids.chunks(FETCH_BATCH) {
            for message in self
                .mail
                .fetch(&folder.name, batch)
                .await
                .context(&context)?
            {
                self.train_one(folder, &message)?;
                summary.trained += 1;
            }
            self.mail
                .add_keyword(&folder.name, batch, keyword::TRAINED)
                .await
                .context(&context)?;
            self.mail
                .move_messages(&folder.name, batch, &folder.dest)
                .await
                .context(&context)?;
        }
        Ok(())
    }

    fn train_one(&self, folder: &TrainFolder, message: &Message) -> Result<()> {
        let tokens = self.tokens(&message.raw)?;
        if folder.label == Label::Good {
            return self.learn_good(&folder.name, message, &tokens, Reason::TrainFolder);
        }

        self.store
            .train(&training_key(message), &tokens, Label::Spam)
            .context("train spam")?;
        let score = self.rescore(&message.raw).context("train spam")?;
        let judgement = Judgement {
            verdict: Verdict::Spam,
            reason: Reason::TrainFolder,
            score,
        };
        self.record(&folder.name, message, &judgement, &Action::TrainSpam, true)
    }

    /// Whitelists the addresses other than the account's own and returns them.
    pub(super) fn whitelist_recipients(&self, addresses: &[String]) -> Result<Vec<String>> {
        let mut added = Vec::new();
        for address in addresses {
            if address.eq_ignore_ascii_case(&self.cfg.account.user) {
                continue;
            }
            self.store
                .add_address(address, Reason::Sent.as_str())
                .context("whitelist recipients")?;
            added.push(address.clone());
        }
        Ok(added)
    }

    /// Whitelists the To and Cc recipients of a message you sent, and learns
    /// the message it replies to.
    pub(super) async fn handle_sent(&mut self, folder: &str, message: &Message) -> Result<()> {
        let Some(parsed) = MessageParser::default().parse_headers(&message.raw) else {
            tracing::debug!(folder, uid = message.uid, "unreadable sent message");
            return Ok(());
        };

        let addresses: Vec<String> = [parsed.to(), parsed.cc()]
            .into_iter()
            .flatten()
            .flat_map(|list| list.mailboxes())
            .filter_map(|mailbox| mailbox.address())
            .map(str::to_lowercase)
            .collect();
        let added = self.whitelist_recipients(&addresses)?;
        if !added.is_empty() {
            // `from` holds the recipients: the sender is always you.
            self.store
                .record_decision(&Decision {
                    at: Timestamp::now(),
                    mailbox: folder.to_owned(),
                    uid: message.uid,
                    message_id: message.message_id.clone(),
                    from: added.join(", "),
                    subject: message.subject.clone(),
                    verdict: Verdict::Good,
                    reason: Reason::Sent,
                    applied: true,
                    ..Decision::default()
                })
                .context("record sent")?;
        }

        let Some(replied) = parsed.in_reply_to().and_then(|ids| ids.last()) else {
            return Ok(());
        };
        let replied = format!("<{}>", replied.trim_matches(['<', '>']));

        if let Err(err) = self.learn_replied(&replied).await {
            tracing::warn!(folder, uid = message.uid, %err, "not learning from reply");
        }
        Ok(())
    }

    /// Learns as good the message you replied to, when tamis had called it
    /// spam or was unsure: answering it says it isn't spam. The message may be
    /// in the folder it was judged in or, if filed as spam, in Junk.
    async fn learn_replied(&mut self, message_id: &str) -> Result<()> {
        let decision = self
            .store
            .latest_decision(message_id)
            .context("look up decision")?;
        let Some(decision) = decision.filter(|d| {
            self.cfg.folders.watch.contains(&d.mailbox)
                && matches!(d.verdict.as_str(), "spam" | "uncertain")
        }) else {
            return Ok(());
        };

        for folder in [decision.mailbox.clone(), self.cfg.folders.junk.clone()] {
            let messages = match self.locate(&folder, message_id).await {
                Err(err) if err.is_message_gone() => continue,
                other => other?,
            };
            let tokens = self.tokens(&messages[0].raw)?;
            return self.learn_good(&folder, &messages[0], &tokens, Reason::Correction);
        }
        Ok(())
    }

    /// Fetches the messages in `folder` carrying `message_id`.
    pub(super) async fn locate(&mut self, folder: &str, message_id: &str) -> Result<Vec<Message>> {
        let gone = || Error::MessageGone {
            message_id: message_id.to_owned(),
            folder: folder.to_owned(),
        };

        let uids = self
            .mail
            .find_message_id(folder, message_id)
            .await
            .context(format!("find message in {folder}"))?;
        if uids.is_empty() {
            return Err(gone());
        }
        let messages = self
            .mail
            .fetch(folder, &uids)
            .await
            .context(format!("fetch from {folder}"))?;
        if messages.is_empty() {
            return Err(gone());
        }
        Ok(messages)
    }
}

//! The sync loop: it judges new mail in the watched folders, files it (in
//! live mode) and journals every decision.

mod authid;
mod bootstrap;
mod domain;
mod error;
mod features;
mod filing;
mod junk;
mod keywords;
mod learn;
mod rescue;
mod train;
mod undo;

use std::fmt::Write as _;
use std::sync::Arc;

use jiff::Timestamp;
use sha2::{Digest, Sha256};

pub use authid::{AuthPin, AuthSource, NO_AUTH_SERVER_ID, resolve_auth_pin};
pub use bootstrap::{BootstrapOptions, BootstrapSummary, Source};
pub use domain::{Action, Judgement, Reason, Summary, Verdict, keyword};
pub use error::{Context, Error, Result};

use crate::auth;
use crate::classify::{self, Label};
use crate::config::{Config, Mode};
use crate::mail::{Mailbox, Message, State};
use crate::rules::Rules;
use crate::store::{Cursor, Decision, Store};

const FETCH_BATCH: usize = 50;

/// Which handler a scanned folder's messages go to.
#[derive(Clone, Copy)]
enum Kind {
    Watched,
    Sent,
    Junk,
}

/// Runs passes over one account.
pub struct Engine<M: Mailbox> {
    mail: M,
    store: Arc<Store>,
    cfg: Arc<Config>,
    rules: Option<Rules>,
    /// The authserv-id to pin to, once resolved.
    auth_id: Option<String>,
}

impl<M: Mailbox> Engine<M> {
    /// An engine for the account behind `mailbox`.
    pub fn new(mailbox: M, store: Arc<Store>, cfg: Arc<Config>) -> Self {
        Self {
            mail: mailbox,
            store,
            cfg,
            rules: None,
            auth_id: None,
        }
    }

    /// Gives the connection back, to close it politely.
    pub fn into_mailbox(self) -> M {
        self.mail
    }

    /// Files good mail into the folders `rules` picks; without it nothing is
    /// filed.
    pub fn set_rules(&mut self, rules: Rules) {
        self.rules = Some(rules);
    }

    /// Judges new mail in each watched folder.
    ///
    /// # Errors
    /// Fails when the mailbox or the store does; `Error::is_connection` says
    /// which kind.
    pub async fn run_once(&mut self) -> Result<Summary> {
        let mut summary = Summary::default();

        for folder in self.train_folders() {
            self.train(&folder, &mut summary).await?;
        }

        for folder in self.cfg.folders.watch.clone() {
            self.scan(&folder, Kind::Watched, &mut summary).await?;
        }

        let sent = self.cfg.folders.sent.clone();
        if let Err(err) = self.scan(&sent, Kind::Sent, &mut summary).await {
            if err.is_connection() {
                return Err(err);
            }
            tracing::warn!(%err, "not learning recipients from Sent");
        }

        if self.wants_junk() {
            let junk = self.cfg.folders.junk.clone();
            self.scan(&junk, Kind::Junk, &mut summary).await?;
        }

        if self.cfg.policy.learn_from_keywords {
            self.learn_from_keywords(&mut summary).await?;
        }
        Ok(summary)
    }

    /// Hands the folder's messages newer than its cursor to the handler. A
    /// missing or stale cursor (new `UIDVALIDITY`) restarts the folder from its
    /// current mail.
    async fn scan(&mut self, folder: &str, kind: Kind, summary: &mut Summary) -> Result<()> {
        let context = format!("scan {folder}");
        let state = self.mail.state(folder).await.context(&context)?;
        let cursor = self.store.cursor(folder).context(&context)?;

        match cursor {
            Some(cursor) if cursor.uid_validity == state.uid_validity => {
                self.scan_new(folder, state.uid_next.max(1) - 1, cursor, kind, summary)
                    .await
            }
            _ => {
                tracing::debug!(folder, "starting folder from current mail");
                self.start_at_current(folder, state)
            }
        }
    }

    /// Handles the messages after the cursor, if the folder's last used UID
    /// says there are any. UIDs up to `last_used` that the search doesn't
    /// return are gone for good; leaving the cursor behind them makes IDLE
    /// report them as new forever.
    async fn scan_new(
        &mut self,
        folder: &str,
        last_used: u32,
        mut cursor: Cursor,
        kind: Kind,
        summary: &mut Summary,
    ) -> Result<()> {
        if cursor.last_uid >= last_used {
            return Ok(());
        }

        let context = format!("scan {folder}");
        let uids = self
            .mail
            .uids_after(folder, cursor.last_uid)
            .await
            .context(&context)?;
        for batch in uids.chunks(FETCH_BATCH) {
            self.scan_batch(folder, batch, &mut cursor, kind, summary)
                .await?;
        }

        if cursor.last_uid >= last_used {
            return Ok(());
        }
        cursor.last_uid = last_used;
        self.store.set_cursor(folder, cursor).context(&context)
    }

    /// Handles one fetch's worth of messages, saving the cursor after each so
    /// an interrupted pass resumes where it stopped.
    async fn scan_batch(
        &mut self,
        folder: &str,
        batch: &[u32],
        cursor: &mut Cursor,
        kind: Kind,
        summary: &mut Summary,
    ) -> Result<()> {
        let context = format!("scan {folder}");
        let messages = self.mail.fetch(folder, batch).await.context(&context)?;

        for message in &messages {
            match kind {
                Kind::Watched => self.handle(folder, message, summary).await,
                Kind::Sent => self.handle_sent(folder, message).await,
                Kind::Junk => self.handle_junk(message, summary).await,
            }
            .context(format!("{folder} UID {}", message.uid))?;

            cursor.last_uid = cursor.last_uid.max(message.uid);
            self.store.set_cursor(folder, *cursor).context(&context)?;
        }
        Ok(())
    }

    fn start_at_current(&self, folder: &str, state: State) -> Result<()> {
        let cursor = Cursor {
            uid_validity: state.uid_validity,
            last_uid: state.uid_next.max(1) - 1,
        };
        self.store
            .set_cursor(folder, cursor)
            .context(format!("start {folder}"))
    }

    async fn handle(
        &mut self,
        folder: &str,
        message: &Message,
        summary: &mut Summary,
    ) -> Result<()> {
        if let Some(why) = self.why_skip(message)? {
            tracing::debug!(folder, uid = message.uid, why, "skipping");
            return Ok(());
        }

        let (tokens, model) = self.features(&message.raw)?;
        if self
            .learn_correction(folder, message, &tokens, summary)
            .await?
        {
            return Ok(());
        }

        let verdict = self.judge(message, &tokens, &model)?;
        summary.scanned += 1;
        summary.count(verdict.verdict);

        if let Some(target) = self.rule_target(folder, message, &verdict).await {
            return self.file_by_rule(folder, message, verdict, &target).await;
        }

        let (action, applied) = self.act(folder, message.uid, verdict.verdict).await?;
        self.record(folder, message, &verdict, &action, applied)
    }

    /// Says why a message needs no judging, or `None` if it does.
    fn why_skip(&self, message: &Message) -> Result<Option<&'static str>> {
        let trained = self
            .store
            .trained(&training_key(message))
            .context("check training")?
            .is_some();
        let sorted = self.sorted_by_tamis(message)?;
        Ok(skip_reason(message, trained, sorted))
    }

    /// Learns from a message the user moved out of Junk, which tamis put there
    /// as spam. Reports whether the message was one.
    async fn learn_correction(
        &mut self,
        folder: &str,
        message: &Message,
        tokens: &[String],
        summary: &mut Summary,
    ) -> Result<bool> {
        if !message.has_flag(keyword::FILED) || !self.moved_by_tamis(message, Verdict::Spam)? {
            return Ok(false);
        }

        // Left on, the keyword would hide a later move back to Junk from the
        // missed-spam check.
        self.mail
            .remove_keyword(folder, &[message.uid], keyword::FILED)
            .await
            .context("clear filed keyword")?;

        summary.scanned += 1;
        summary.corrected += 1;
        self.learn_good(folder, message, tokens, Reason::Correction)?;
        Ok(true)
    }

    /// Reports whether the message was filed by a rule and then put back by
    /// the user, which is when it must be left where it is. The keyword alone
    /// doesn't say so: it is set before the move, and a move cut short by a
    /// dropped connection leaves it on a message that was never filed. So the
    /// journal has to show the move too. Without a Message-ID there is nothing
    /// to look up, and the keyword is trusted.
    fn sorted_by_tamis(&self, message: &Message) -> Result<bool> {
        if !message.has_flag(keyword::SORTED) {
            return Ok(false);
        }
        if message.message_id.is_empty() {
            return Ok(true);
        }
        self.moved_by_tamis(message, Verdict::Good)
    }

    /// Trusts the whitelist when the sender is (if required) verified, and
    /// otherwise scores the message with the Bayes model.
    fn judge(
        &mut self,
        message: &Message,
        tokens: &[String],
        model: &impl classify::Model,
    ) -> Result<Judgement> {
        if !message.from.is_empty() && self.store.has_address(&message.from).context("judge")? {
            let verified = !self.cfg.policy.whitelist_requires_auth || {
                let server_id = self.auth_server_id();
                auth::verified(&message.raw, &server_id)
            };
            if verified {
                return Ok(Judgement {
                    verdict: Verdict::Good,
                    reason: Reason::Whitelisted,
                    score: 0.0,
                });
            }
        }

        let score = classify::score(model, tokens).score;
        let policy = &self.cfg.policy;
        let verdict = if score >= policy.spam_threshold {
            Verdict::Spam
        } else if score <= policy.good_threshold {
            Verdict::Good
        } else {
            Verdict::Uncertain
        };
        Ok(Judgement {
            verdict,
            reason: Reason::Bayes,
            score,
        })
    }

    /// Reports whether the journal shows tamis moved this message with the
    /// given verdict. A keyword alone doesn't: a move that failed on a dropped
    /// connection leaves its keyword behind.
    fn moved_by_tamis(&self, message: &Message, verdict: Verdict) -> Result<bool> {
        let decision = self
            .store
            .latest_decision(&message.message_id)
            .context("look up decision")?;
        Ok(decision.is_some_and(|d| {
            d.applied && d.verdict == verdict && matches!(d.action, Action::Move(_))
        }))
    }

    /// Files a message according to its verdict; in shadow mode it only
    /// reports what it would have done.
    async fn act(&mut self, folder: &str, uid: u32, verdict: Verdict) -> Result<(Action, bool)> {
        let live = self.cfg.policy.mode == Mode::Live;
        let uids = [uid];

        match verdict {
            Verdict::Spam => {
                let junk = self.cfg.folders.junk.clone();
                let action = Action::Move(junk.clone());
                if !live {
                    return Ok((action, false));
                }

                self.mail
                    .add_keyword(folder, &uids, keyword::FILED)
                    .await
                    .context("file spam")?;
                if let Err(err) = self.mail.move_messages(folder, &uids, &junk).await {
                    // Left in place with the keyword, the next pass would
                    // mistake the message for one the user moved back out of
                    // Junk.
                    if let Err(undo) = self
                        .mail
                        .remove_keyword(folder, &uids, keyword::FILED)
                        .await
                    {
                        tracing::debug!(%undo, "could not clear the filed keyword");
                    }
                    return Err(err).context("file spam");
                }
                Ok((action, true))
            }
            Verdict::Uncertain => {
                let action = Action::Keyword(keyword::UNCERTAIN.to_owned());
                if !live {
                    return Ok((action, false));
                }
                self.mail
                    .add_keyword(folder, &uids, keyword::UNCERTAIN)
                    .await
                    .context("flag uncertain")?;
                Ok((action, true))
            }
            Verdict::Good => Ok((Action::None, false)),
        }
    }

    /// Trains a message as good and whitelists its sender: the user has said
    /// it isn't spam.
    fn learn_good(
        &self,
        folder: &str,
        message: &Message,
        tokens: &[String],
        reason: Reason,
    ) -> Result<()> {
        self.store
            .train(&training_key(message), tokens, Label::Good)
            .context("learn good")?;
        if !message.from.is_empty() {
            self.store
                .add_address(&message.from, reason.as_str())
                .context("learn good")?;
        }

        let score = self.rescore(&message.raw).context("learn good")?;
        let judgement = Judgement {
            verdict: Verdict::Good,
            reason,
            score,
        };
        self.record(folder, message, &judgement, &Action::TrainGood, true)
    }

    /// Trains a message as spam and takes its sender off the whitelist: the
    /// user has said it is spam.
    fn learn_spam(
        &self,
        folder: &str,
        message: &Message,
        tokens: &[String],
        reason: Reason,
    ) -> Result<()> {
        self.store
            .train(&training_key(message), tokens, Label::Spam)
            .context("learn spam")?;
        if !message.from.is_empty() {
            self.store
                .remove_address(&message.from)
                .context("learn spam")?;
        }

        let score = self.rescore(&message.raw).context("learn spam")?;
        let judgement = Judgement {
            verdict: Verdict::Spam,
            reason,
            score,
        };
        self.record(folder, message, &judgement, &Action::TrainSpam, true)
    }

    fn record(
        &self,
        folder: &str,
        message: &Message,
        verdict: &Judgement,
        action: &Action,
        applied: bool,
    ) -> Result<()> {
        tracing::debug!(
            folder,
            uid = message.uid,
            from = %message.from,
            subject = %message.subject,
            verdict = verdict.verdict.as_str(),
            reason = verdict.reason.as_str(),
            score = verdict.score,
            %action,
            applied,
            "decision"
        );

        self.store
            .record_decision(&Decision {
                at: Timestamp::now(),
                mailbox: folder.to_owned(),
                uid: message.uid,
                message_id: message.message_id.clone(),
                from: message.from.clone(),
                subject: message.subject.clone(),
                score: verdict.score,
                verdict: verdict.verdict,
                reason: verdict.reason,
                action: action.clone(),
                applied,
                ..Decision::default()
            })
            .context("record decision")?;
        Ok(())
    }
}

/// Says why a message needs no judging, or `None` if it does. `sorted` is
/// whether a rule filed it and the user put it back.
fn skip_reason(message: &Message, trained: bool, sorted: bool) -> Option<&'static str> {
    if trained && message.has_flag(keyword::TRAINED) {
        Some("already trained")
    } else if message.has_flag(keyword::RESCUED) {
        Some("rescued by tamis")
    } else if sorted {
        Some("sorted by tamis")
    } else {
        None
    }
}

/// Identifies a message across folders: its Message-ID, or a hash of its
/// content when it has none.
fn training_key(message: &Message) -> String {
    if message.message_id.is_empty() {
        let digest = Sha256::digest(&message.raw);
        digest
            .iter()
            .fold(String::from("sha256:"), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    } else {
        message.message_id.clone()
    }
}

#[cfg(test)]
mod tests;
